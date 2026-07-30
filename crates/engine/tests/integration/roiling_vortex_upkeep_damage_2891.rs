//! Roiling Vortex — "At the beginning of each player's upkeep, this enchantment
//! deals 1 damage to them." deals the damage to the player whose upkeep it is.
//!
//! Regression for issue #2891: the bare player anaphor "them" was parsed as an
//! object anaphor (`ParentTarget`) with no referent, so the upkeep trigger fired
//! and logged but dealt 0 damage to anyone. Per CR 603.2b, "them" is the active
//! player whose upkeep triggered the ability; the parser now binds it to
//! `ScopedPlayer`, which the runtime resolves to that player at fire time.
//!
//! Discriminating end-to-end: starting from P0's pre-combat main, the next
//! upkeep reached is P1's, so the trigger must deal 1 damage to P1 (the active
//! player) and none to P0. Pre-fix both players are untouched.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const ORACLE: &str =
    "At the beginning of each player's upkeep, this enchantment deals 1 damage to them.";

#[test]
fn roiling_vortex_deals_upkeep_damage_to_the_active_player() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Stock libraries so draw steps never deck anyone out before the assertion.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    // Roiling Vortex on P0's battlefield, parsed from real Oracle text. The card
    // type is irrelevant to the upkeep trigger under test, so a 0/1 body keeps
    // the scenario minimal.
    scenario.add_creature_from_oracle(P0, "Roiling Vortex", 0, 1, ORACLE);

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    // Roll the turn forward from P0's pre-combat main into P1's upkeep, passing
    // priority and declaring no attackers/blockers so combat never stalls. The
    // upkeep trigger resolves on a priority pass (it is non-targeted), so stop
    // once P1's life changes — or once the bound is hit (then the assertion
    // reports the failure).
    for _ in 0..400 {
        if runner.life(P1) != p1_before {
            break;
        }
        let acted = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            _ => break,
        };
        if acted.is_err() {
            break;
        }
    }

    assert_eq!(
        runner.life(P1),
        p1_before - 1,
        "Roiling Vortex must deal 1 damage to the upkeep player (the active player P1)"
    );
    assert_eq!(
        runner.life(P0),
        p0_before,
        "the non-active player takes no damage on another player's upkeep"
    );
}

//! Gideon of the Trials' emblem — conditional game-outcome locks (issue-free
//! coverage gap: `Swallow:Condition_AsLongAs`).
//!
//! Emblem text (verbatim MTGJSON, the third loyalty ability's payload):
//!   "As long as you control a Gideon planeswalker, you can't lose the game
//!    and your opponents can't win the game."
//!
//! These tests drive the REAL pipeline: the emblem-granting clause is parsed
//! at test time by the production parser (so reverting the compound
//! cant-win/lose multi arm or the emblem multi-static upgrade fails every
//! test here), the emblem is created by resolving a cast spell through
//! `GameRunner::cast(..).resolve()`, and the locks are enforced by the SBA /
//! win-lose seams that landed with Angel's Grace (PR #6093).
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 114.1 + CR 114.4: emblems live in the command zone and their
//!     abilities function there.
//!   - CR 109.4c: an emblem is controlled by the player who put it into the
//!     command zone — the condition's "you" binds to that player.
//!   - CR 104.3b-e: loss by SBA / effect is precluded by "can't lose the
//!     game"; CR 104.2b: an effect-stated win is precluded by "can't win the
//!     game" (CR 810.8a is the printed effect language).
//!   - CR 611.3a: the "as long as" gate is live — re-evaluated whenever the
//!     statics are read, so the protection evaporates the moment no Gideon
//!     planeswalker is controlled (never latched).
//!   - CR 205.3j: "Gideon" is a planeswalker type.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Verbatim emblem-granting clause of the third loyalty ability. The synthetic
/// spell name shares no words with "Gideon", so the card-name normalizer is
/// deliberately inert here — the normalizer's own revert-failing coverage
/// lives in the `oracle_util` unit tests and the full-card parse test.
const GIDEON_EMBLEM_ORACLE: &str = "You get an emblem with \"As long as you control a Gideon planeswalker, you can't lose the game and your opponents can't win the game.\"";

/// Build a game where P0 has cast a sorcery granting the emblem. When
/// `with_gideon` is set, P0 also controls a permanent with the Planeswalker
/// core type and the Gideon planeswalker type (CR 205.3j), satisfying the
/// emblem's condition.
fn setup_with_emblem(with_gideon: bool) -> (GameRunner, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Emblem Grant", false, GIDEON_EMBLEM_ORACLE)
        .id();
    let gideon = with_gideon.then(|| scenario.add_creature(P0, "Test Walker", 4, 4).id());
    let mut runner = scenario.build();
    if let Some(id) = gideon {
        // CR 205.3j: stamp the planeswalker type + Gideon planeswalker type.
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.card_types.subtypes.push("Gideon".to_string());
        obj.base_card_types = obj.card_types.clone();
    }
    runner.cast(spell).resolve();
    (runner, gideon)
}

/// Shape guard shared by every test: the resolved spell must have produced a
/// command-zone emblem carrying BOTH conditioned locks. This is what makes
/// the negative assertions below non-vacuous — the emblem demonstrably
/// exists and parsed to the full two-static shape.
fn assert_emblem_shape(runner: &GameRunner) {
    let state = runner.state();
    let emblem = state
        .command_zone
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_emblem)
        .expect("resolving the spell must create a command-zone emblem (CR 114.1)");
    let modes: Vec<&StaticMode> = emblem
        .static_definitions
        .iter_unchecked()
        .map(|d| &d.mode)
        .collect();
    assert!(
        modes.contains(&&StaticMode::CantLoseTheGame)
            && modes.contains(&&StaticMode::CantWinTheGame),
        "emblem must carry both outcome locks, got {modes:?}"
    );
    assert!(
        emblem
            .static_definitions
            .iter_unchecked()
            .all(|d| d.condition.is_some()),
        "both locks must be condition-gated (CR 611.3a)"
    );
}

/// CR 104.3b + CR 114.4: while its controller controls a Gideon planeswalker,
/// the emblem precludes the 0-or-less-life loss SBA — and ONLY for its
/// controller: the unprotected opponent at 0 life IS eliminated, proving the
/// loss SBA executed (reach guard) and the You-scoped filter is per-player.
#[test]
fn emblem_blocks_loss_sba_while_controlling_gideon_planeswalker() {
    let (mut runner, _gideon) = setup_with_emblem(true);
    assert_emblem_shape(&runner);

    runner.state_mut().players[0].life = 0;
    runner.state_mut().players[1].life = 0;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        !runner.state().players[0].is_eliminated,
        "emblem's controller at 0 life must not lose while controlling a Gideon planeswalker"
    );
    assert!(
        runner.state().players[1].is_eliminated,
        "unprotected opponent at 0 life must still be eliminated (reach guard)"
    );
}

/// CR 611.3a: the gate is LIVE — once the Gideon planeswalker leaves the
/// battlefield, the same loss SBA eliminates the emblem's controller. This
/// discriminates against both an unconditional parse and a latched condition.
#[test]
fn emblem_stops_protecting_after_gideon_leaves() {
    let (mut runner, gideon) = setup_with_emblem(true);
    assert_emblem_shape(&runner);
    let gideon = gideon.expect("setup placed a Gideon planeswalker");

    // The Gideon planeswalker dies; the emblem stays (CR 114.4), but its
    // condition is no longer met.
    engine::game::zones::move_to_zone(runner.state_mut(), gideon, Zone::Graveyard, &mut Vec::new());

    runner.state_mut().players[0].life = 0;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        runner.state().players[0].is_eliminated,
        "with no Gideon planeswalker controlled, the emblem's condition fails and the loss SBA applies (CR 611.3a)"
    );
}

/// CR 104.2b: while the condition holds, an opponent's "You win the game."
/// effect resolved through the real cast pipeline does not end the game.
#[test]
fn emblem_blocks_opponent_win_effect_while_condition_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Emblem Grant", false, GIDEON_EMBLEM_ORACLE)
        .id();
    let win = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Win Spell", true, "You win the game.")
        .id();
    let gideon = scenario.add_creature(P0, "Test Walker", 4, 4).id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&gideon).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.card_types.subtypes.push("Gideon".to_string());
        obj.base_card_types = obj.card_types.clone();
    }
    runner.cast(grant).resolve();
    assert_emblem_shape(&runner);

    // CR 117.3d: the active player (P0) passes priority so P1 can cast.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 can cast");
    runner.cast(win).resolve();

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "opponent's win effect must be precluded while the emblem's condition holds (CR 104.2b), got {:?}",
        runner.state().waiting_for
    );
}

/// Reach-guard sibling for the win-block test: with the emblem present but NO
/// Gideon planeswalker controlled, the identical win spell ends the game —
/// the win path is live and the block above genuinely came from the
/// condition-gated emblem static.
#[test]
fn opponent_win_effect_resolves_when_condition_fails() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Emblem Grant", false, GIDEON_EMBLEM_ORACLE)
        .id();
    let win = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Win Spell", true, "You win the game.")
        .id();
    let mut runner = scenario.build();
    runner.cast(grant).resolve();
    assert_emblem_shape(&runner);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 can cast");
    runner.cast(win).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "with no Gideon planeswalker controlled, the win effect resolves (reach guard), got {:?}",
        runner.state().waiting_for
    );
}

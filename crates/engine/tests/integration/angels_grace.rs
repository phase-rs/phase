//! CR 104.2b + CR 104.3b + CR 104.3e + CR 614.1a + CR 514.2 — Angel's Grace.
//!
//! "Split second (…)\nYou can't lose the game this turn and your opponents
//! can't win the game this turn. Until end of turn, damage that would reduce
//! your life total to less than 1 reduces it to 1 instead."
//!
//! Three runtime seams, each with a revert-failing assertion:
//! - the conjunct splitter arm ("… and your opponents can't …") that lets the
//!   sentence lower to `CantLoseTheGame`/`CantWinTheGame` transient effects
//!   (without it the whole sentence is `Effect::Unimplemented` and every
//!   assertion here fails);
//! - the life-floor lift to `Effect::AddTargetReplacement` installing a
//!   floating, non-consumed, turn-bound `DamageModification::LifeFloor`
//!   replacement (CR 614.1a);
//! - the floating replacement's `DamageTargetPlayerScope::Controller`
//!   authority: it must bind to the INSTALLING player (`source_controller`),
//!   not a hardcoded `PlayerId(0)` — which is why every test here casts
//!   Angel's Grace from **P1**. A P0-caster fixture would pass vacuously
//!   against that hardcode.
//!
//! Oracle text is verbatim (per the `/card-test` recipe); the Split second
//! keyword line is fed through the keyword path, mirroring the MTGJSON
//! pipeline, so it never degrades to an inline-reminder `Unimplemented`.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Verbatim Oracle text (MTGJSON), including the Split second keyword line.
const ANGELS_GRACE: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nYou can't lose the game this turn and your opponents can't win the game this turn. Until end of turn, damage that would reduce your life total to less than 1 reduces it to 1 instead.";

/// Add Angel's Grace to P1's hand with Split second fed via the keyword path.
fn add_angels_grace(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_spell_to_hand(P1, "Angel's Grace", /* is_instant */ true);
    builder.from_oracle_text_with_keywords(&["Split second"], ANGELS_GRACE);
    builder.id()
}

/// CR 117.3d: P0 (the active player) passes priority so P1 can cast the
/// instant, then drive the full cast pipeline to resolution.
fn cast_angels_grace_as_p1(runner: &mut GameRunner, spell: ObjectId) {
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 can cast");
    runner.cast(spell).resolve();
}

/// CR 104.3b: after Angel's Grace resolves, its caster (P1) is not eliminated
/// by the 0-or-less-life SBA this turn, while the unprotected opponent (P0) at
/// 0 life IS eliminated — the live sibling proves the loss SBA path executed
/// (reach guard), so the P1 survival is not vacuous.
#[test]
fn angels_grace_controller_cannot_lose_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_angels_grace(&mut scenario);
    let mut runner = scenario.build();

    cast_angels_grace_as_p1(&mut runner, spell);

    runner.state_mut().players[0].life = 0;
    runner.state_mut().players[1].life = 0;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        !runner.state().players[1].is_eliminated,
        "Angel's Grace caster at 0 life must not lose this turn (CR 104.3b skip)"
    );
    assert!(
        runner.state().players[0].is_eliminated,
        "unprotected opponent at 0 life must still be eliminated (reach guard)"
    );
}

/// CR 104.2b: after Angel's Grace resolves, an opponent's "You win the game."
/// effect does not end the game this turn.
#[test]
fn angels_grace_blocks_opponent_win_effect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grace = add_angels_grace(&mut scenario);
    let win = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Win Spell", true, "You win the game.")
        .id();
    let mut runner = scenario.build();

    cast_angels_grace_as_p1(&mut runner, grace);
    // Priority returns to the active player (P0), who now tries to win.
    runner.cast(win).resolve();

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "opponent's win effect must be blocked by Angel's Grace (CR 104.2b), got {:?}",
        runner.state().waiting_for
    );
}

/// CR 104.3e + CR 810.8a: an effect-stated loss ("Target player loses the
/// game" — Door to Nothingness wording) is precluded for the Angel's Grace
/// caster this turn, while the same effect still eliminates the unprotected
/// opponent (in-test reach guard proving the effect-loss path is live).
#[test]
fn angels_grace_blocks_loss_effects_against_caster() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grace = add_angels_grace(&mut scenario);
    let doom_a = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Loss Spell A",
            true,
            "Target player loses the game.",
        )
        .id();
    let doom_b = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Loss Spell B",
            true,
            "Target player loses the game.",
        )
        .id();
    let mut runner = scenario.build();

    cast_angels_grace_as_p1(&mut runner, grace);

    // P0 (active, holding priority post-resolution) aims the loss at the
    // protected caster: the effect resolves but the loss is precluded.
    runner.cast(doom_a).target_player(P1).resolve();
    assert!(
        !runner.state().players[1].is_eliminated,
        "Angel's Grace caster must not lose to an effect-stated loss (CR 104.3e)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "game must continue after the blocked loss"
    );

    // Reach guard: the identical effect against the unprotected P0 eliminates
    // them, proving the effect-loss path is live this game.
    runner.cast(doom_b).target_player(P0).resolve();
    assert!(
        runner.state().players[0].is_eliminated,
        "unprotected player must still lose to the effect (reach guard)"
    );
}

/// Reach guard for [`angels_grace_blocks_opponent_win_effect`]: without Angel's
/// Grace the identical win effect ends the game, proving the win path is live
/// and the blocked outcome above is not vacuous.
#[test]
fn win_effect_ends_game_without_angels_grace() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let win = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Win Spell", true, "You win the game.")
        .id();
    let mut runner = scenario.build();

    runner.cast(win).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "without Angel's Grace the win effect must end the game, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 614.1 + CR 614.3: replacement effects apply continuously as events
/// happen and last until their duration expires — the life floor applies to
/// EVERY damage event this turn (not a consumed one-shot shield), floors only
/// the CASTER's life (P1), and does not floor the opponent's.
///
/// Both players start at 2 life so the scope assertions discriminate the
/// controller authority: under the old hardcoded-`PlayerId(0)` floating
/// target-filter check, P1's damage would NOT be floored (life -1) and P0's
/// wrongly would — both assertions flip.
#[test]
fn angels_grace_life_floor_applies_to_each_damage_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 2).with_life(P1, 2);
    let grace = add_angels_grace(&mut scenario);
    let bolt_a = scenario.add_bolt_to_hand(P0);
    let bolt_b = scenario.add_bolt_to_hand(P0);
    let bolt_c = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();

    cast_angels_grace_as_p1(&mut runner, grace);

    // First damage event: 3 damage at 2 life is floored to 1 (CR 614.1a).
    let outcome = runner.cast(bolt_a).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -1);

    // Second damage event the same turn: still floored — the replacement is
    // continuous for the turn, not consumed on first use.
    let outcome = runner.cast(bolt_b).target_player(P1).resolve();
    outcome.assert_life_delta(P1, 0);
    assert_eq!(
        runner.state().players[1].life,
        1,
        "every damage event this turn is floored at 1"
    );

    // Multi-authority scope: the floor binds to the caster (P1), never the
    // opponent — P0's own damage is NOT floored, dropping P0 to -1 and losing
    // the game to the SBA (which also re-proves the damage pipeline is live).
    let outcome = runner.cast(bolt_c).target_player(P0).resolve();
    outcome.assert_life_delta(P0, -3);
    assert_eq!(
        runner.state().players[0].life,
        -1,
        "the floor must not apply to the non-caster (Controller scope authority)"
    );
}

/// CR 514.2: the floor (and the can't-lose lock) end at cleanup — next turn
/// the same damage is not floored, so the caster's life drops below 1.
#[test]
fn angels_grace_life_floor_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 2);
    let grace = add_angels_grace(&mut scenario);
    let bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    cast_angels_grace_as_p1(&mut runner, grace);

    // Cross the cleanup step into P1's turn (CR 514.2 prunes the floor).
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "scenario must have advanced into P1's turn"
    );

    // P1 (active, holding priority) bolts themself: 3 damage at 2 life with no
    // floor drops them to -1. If the floor (or its expiry) leaked past
    // cleanup, life would read 1 instead.
    runner.cast(bolt).target_player(P1).resolve();
    assert_eq!(
        runner.state().players[1].life,
        -1,
        "the life floor must expire at cleanup (CR 514.2)"
    );
    // CR 514.2 + CR 104.3b: the can't-lose transient also ended at cleanup, so
    // the loss SBA now eliminates P1 — proving BOTH halves of "this turn"
    // expired, not just the replacement.
    assert!(
        runner.state().players[1].is_eliminated,
        "the can't-lose effect must also expire at cleanup (CR 514.2)"
    );
}

//! Regression tests for GitHub issue #508 — Belbe, Corrupted Observer and
//! Thornbow Archer life-loss behavior.
//!
//! Both bugs were parser bugs (the life-loss tracking primitive itself is
//! correct):
//!
//! 1. **Belbe** — "the active player adds {C}{C} for each of your opponents
//!    who lost life this turn" parsed to `Effect::Unimplemented` because the
//!    mana dispatcher only fired on a bare `add ` prefix; the `{C}{C}` ×2
//!    multiplier was also dropped. After the fix the trigger body lowers to
//!    `Effect::Mana` with a `Multiply { factor: 2, inner: Ref { PlayerCount {
//!    OpponentLostLife } } }` colorless count, routed to the active player.
//!
//! 2. **Thornbow Archer** — "each opponent who doesn't control an Elf loses 1
//!    life" silently dropped the "who doesn't control an Elf" qualifier, so
//!    the effect over-applied to every opponent. After the fix the trigger
//!    carries `PlayerFilter::ControlsCount { <Elf>, EQ, Fixed(0) }`.
//!
//! Both tests drive the full pipeline through `apply` — no synthetic events.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

/// CR 106.4 + CR 505.1 + CR 119.2: Belbe's postcombat-main trigger adds {C}{C}
/// per opponent who lost life this turn, into the active player's mana pool.
/// Two opponents lost life → 4 colorless mana. Reverting the parser fix makes
/// the trigger body `Unimplemented` (0 mana) or drops the ×2 (2 mana).
#[test]
fn belbe_postcombat_main_adds_two_colorless_per_opponent_who_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 99);
    scenario.at_phase(Phase::EndCombat);

    scenario.add_creature_from_oracle(
        P0,
        "Belbe, Corrupted Observer",
        2,
        2,
        "At the beginning of each postcombat main phase, the active player adds \
         {C}{C} for each of your opponents who lost life this turn.",
    );

    let mut runner = scenario.build();
    // Two opponents lost life this turn; the controller (P0) did not. This is
    // exactly the per-turn state the engine's own life-loss paths maintain
    // (verified correct in the plan's investigation).
    for p in runner.state_mut().players.iter_mut() {
        p.life_lost_this_turn = if p.id == P0 { 0 } else { 3 };
    }
    assert_eq!(
        runner.state().active_player,
        P0,
        "precondition: P0 is the active player"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "precondition: P0's mana pool starts empty"
    );

    // Roll EndCombat → PostCombatMain; the Phase trigger fires on entry.
    // Pass priority for every player (3-player game) until the phase advances.
    for _ in 0..12 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "must have advanced into the postcombat main phase"
    );

    let pool = &runner.state().players[0].mana_pool;
    assert_eq!(
        pool.count_color(ManaType::Colorless),
        4,
        "Belbe adds {{C}}{{C}} per opponent who lost life — 2 opponents → 4 colorless"
    );
    assert_eq!(pool.total(), 4, "only colorless mana is produced");
}

/// CR 119.3: with no opponent having lost life, Belbe adds zero mana — guards
/// the `PlayerCount { OpponentLostLife }` filter.
#[test]
fn belbe_adds_zero_when_no_opponent_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::EndCombat);

    scenario.add_creature_from_oracle(
        P0,
        "Belbe, Corrupted Observer",
        2,
        2,
        "At the beginning of each postcombat main phase, the active player adds \
         {C}{C} for each of your opponents who lost life this turn.",
    );
    // No player lost life this turn (default state).

    let mut runner = scenario.build();
    for _ in 0..12 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "no opponent lost life — Belbe adds no mana"
    );
}

/// CR 109.4 + CR 120.3a: Thornbow Archer's attack trigger drains 1 life from
/// each opponent who does NOT control an Elf. An Elf-controlling opponent is
/// untouched. Reverting the parser fix drops the qualifier, so the
/// Elf-controlling opponent loses life too.
#[test]
fn thornbow_archer_attack_drains_only_elfless_opponents() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let thornbow = scenario
        .add_creature_from_oracle(
            P0,
            "Thornbow Archer",
            1,
            2,
            "Whenever this creature attacks, each opponent who doesn't control \
             an Elf loses 1 life.",
        )
        .id();

    // P1 controls an Elf (suppresses the drain); P2 controls none.
    scenario
        .add_creature(P1, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"]);

    let mut runner = scenario.build();
    assert_eq!(runner.life(P1), 20, "precondition: P1 at 20");
    assert_eq!(runner.life(P2), 20, "precondition: P2 at 20");

    // Pass priority for every player (3-player game) until the active player
    // reaches the declare-attackers step.
    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(thornbow, AttackTarget::Player(P1))],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        20,
        "P1 controls an Elf — Thornbow's drain must NOT apply"
    );
    assert_eq!(
        runner.life(P2),
        19,
        "P2 controls no Elf — Thornbow drains exactly 1 life"
    );
}

/// CR 109.4: when no opponent controls an Elf, the `player_scope` fan-out
/// still drains every Elf-less opponent.
#[test]
fn thornbow_archer_attack_drains_all_when_no_opponent_controls_elf() {
    let mut scenario = GameScenario::new_n_player(3, 13);
    scenario.at_phase(Phase::PreCombatMain);

    let thornbow = scenario
        .add_creature_from_oracle(
            P0,
            "Thornbow Archer",
            1,
            2,
            "Whenever this creature attacks, each opponent who doesn't control \
             an Elf loses 1 life.",
        )
        .id();

    let mut runner = scenario.build();

    // Pass priority for every player (3-player game) until the active player
    // reaches the declare-attackers step.
    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(thornbow, AttackTarget::Player(P1))],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P1), 19, "no Elf — P1 drains 1");
    assert_eq!(runner.life(P2), 19, "no Elf — P2 drains 1");
}

//! Beckett Mariner, Impetuous Ensign — "Whenever Beckett Mariner becomes
//! tapped, put a promotion counter on her. Then if there are three or more
//! promotion counters on her, remove those counters and she deals 2 damage to
//! each opponent."
//!
//! Drives the real parser + trigger pipeline from the verbatim Oracle text and
//! taps Beckett by attacking with her. Covers the gendered self-reference in
//! the counter threshold ("on her", CR 201.5), the gate itself (CR 608.2c), and
//! the "remove those counters" anaphor binding to the counted promotion
//! counters only (CR 608.2k + CR 122.1).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Beckett Mariner, Impetuous Ensign, verbatim (Scryfall Oracle text).
const BECKETT: &str = "First strike\n\
Reckless Behavior — Whenever Beckett Mariner becomes tapped, put a promotion counter on her. \
Then if there are three or more promotion counters on her, remove those counters and she deals \
2 damage to each opponent.";

const P2: PlayerId = PlayerId(2);

fn promotion() -> CounterType {
    parse_counter_type("promotion")
}

/// Beckett with `promotion` promotion counters and one +1/+1 counter attacks
/// P1; returns the runner after her trigger has resolved, plus her id.
fn attack_with_beckett(players: u8, promotion_counters: u32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let beckett = scenario
        .add_creature_from_oracle(P0, "Beckett Mariner, Impetuous Ensign", 2, 2, BECKETT)
        .id();
    scenario.with_counter(beckett, promotion(), promotion_counters);
    scenario.with_counter(beckett, CounterType::Plus1Plus1, 1);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(beckett, AttackTarget::Player(P1))])
        .expect("declare Beckett as an attacker");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the tap trigger resolved: {:?}",
        runner.state().waiting_for
    );
    (runner, beckett)
}

fn counters(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

/// CR 608.2c + CR 608.2k: the third promotion counter meets the threshold, so
/// the promotion counters are removed — but not her +1/+1 counter, which is not
/// among "those counters" — and each opponent takes 2.
#[test]
fn beckett_third_promotion_counter_removes_them_and_damages_each_opponent() {
    let (runner, beckett) = attack_with_beckett(3, 2);
    assert_eq!(
        counters(&runner, beckett, &promotion()),
        0,
        "promotion counters removed"
    );
    assert_eq!(
        counters(&runner, beckett, &CounterType::Plus1Plus1),
        1,
        "the +1/+1 counter is not one of those counters"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "Beckett's controller is not an opponent"
    );
    assert_eq!(runner.life(P1), 18, "2 damage to the defending opponent");
    assert_eq!(runner.life(P2), 18, "2 damage to the other opponent too");
}

/// CR 608.2c: two promotion counters are below the threshold — the counter
/// stays and nobody is dealt damage.
#[test]
fn beckett_below_three_promotion_counters_keeps_them_and_deals_no_damage() {
    let (runner, beckett) = attack_with_beckett(2, 1);
    assert_eq!(
        counters(&runner, beckett, &promotion()),
        2,
        "the tap added a counter and nothing removed it"
    );
    assert_eq!(counters(&runner, beckett, &CounterType::Plus1Plus1), 1);
    assert_eq!(runner.life(P1), 20, "no damage below the threshold");
}

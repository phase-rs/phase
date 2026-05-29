//! Regression coverage for issue #888: Emissary Green's attack-triggered vote.
//!
//! Oracle text:
//! > Whenever Emissary Green attacks, starting with you, each player votes for
//! > profit or security. You create a number of Treasure tokens equal to twice
//! > the number of profit votes. Put a number of +1/+1 counters on each
//! > creature you control equal to the number of security votes.
//!
//! Before the fix the per-choice effects used the aggregate-tally phrasing
//! ("a number of … equal to [twice] the number of <choice> votes"), which the
//! vote parser did not recognize — the trigger degraded to `Effect::Unimplemented`
//! and no vote prompt was ever shown. These tests drive the real
//! combat → Attacks trigger → `WaitingFor::VoteChoice` → tally pipeline and
//! assert the per-vote fan-out math:
//!   * each `profit` vote creates 2 Treasures (the "twice" multiplier), and
//!   * each `security` vote puts one +1/+1 counter on each creature you control.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

const EMISSARY_GREEN_ORACLE: &str = "Whenever Emissary Green attacks, starting with you, \
each player votes for profit or security. You create a number of Treasure tokens equal to \
twice the number of profit votes. Put a number of +1/+1 counters on each creature you control \
equal to the number of security votes.";

/// Count Treasure tokens on the battlefield owned by `player`.
fn treasure_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.owner == player)
        .filter(|obj| obj.name.eq_ignore_ascii_case("Treasure"))
        .count()
}

/// +1/+1 counters currently on `id`.
fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// Build a 2-player board with Emissary Green (from real Oracle text) and a
/// second creature, both controlled by P0, and declare Emissary Green as an
/// attacker. Returns the runner plus the two creature ids.
fn attack_with_emissary() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let emissary = scenario
        .add_creature_from_oracle(P0, "Emissary Green", 3, 3, EMISSARY_GREEN_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![emissary],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
        };
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(emissary, AttackTarget::Player(P1))],
        })
        .expect("declaring Emissary Green as attacker should succeed");
    (runner, emissary, bear)
}

/// Pass priority until the attack-triggered vote prompt appears.
fn advance_to_vote_prompt(runner: &mut GameRunner) {
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }) {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!(
        "Emissary Green's attack must produce a vote prompt, got {:?}",
        runner.waiting_for_kind()
    );
}

/// Submit the supplied choices in voter-queue order. The queue is
/// `[P0, P1]` (AllPlayers, starting with you), so `choices[0]` is P0's vote
/// and `choices[1]` is P1's.
fn cast_votes(runner: &mut GameRunner, choices: [&str; 2]) {
    for choice in choices {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }),
            "expected a VoteChoice prompt before casting '{choice}'"
        );
        runner
            .act(GameAction::ChooseOption {
                choice: choice.to_string(),
            })
            .unwrap_or_else(|e| panic!("casting vote '{choice}' should succeed: {e:?}"));
    }
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }),
        "all votes cast — the tally should have resolved"
    );
}

/// Both players vote `profit` → 2 profit votes × 2 Treasures each = 4 Treasures
/// for the controller; zero security votes → no +1/+1 counters.
#[test]
fn issue_888_both_profit_creates_four_treasures() {
    let (mut runner, emissary, bear) = attack_with_emissary();
    advance_to_vote_prompt(&mut runner);
    cast_votes(&mut runner, ["profit", "profit"]);

    assert_eq!(
        treasure_count(&runner, P0),
        4,
        "two profit votes × twice = 4 Treasures for the controller"
    );
    assert_eq!(treasure_count(&runner, P1), 0, "opponent gets no Treasures");
    assert_eq!(plus_counters(&runner, emissary), 0);
    assert_eq!(plus_counters(&runner, bear), 0);
}

/// Both players vote `security` → 2 security votes → each creature you control
/// gains 2 +1/+1 counters; zero profit votes → no Treasures.
#[test]
fn issue_888_both_security_adds_counters_to_each_creature() {
    let (mut runner, emissary, bear) = attack_with_emissary();
    advance_to_vote_prompt(&mut runner);
    cast_votes(&mut runner, ["security", "security"]);

    assert_eq!(
        treasure_count(&runner, P0),
        0,
        "no profit votes → no Treasures"
    );
    assert_eq!(
        plus_counters(&runner, emissary),
        2,
        "two security votes → 2 counters on Emissary Green"
    );
    assert_eq!(
        plus_counters(&runner, bear),
        2,
        "two security votes → 2 counters on the other creature you control"
    );
}

/// Split vote: P0 profit, P1 security → 1 profit vote (2 Treasures) and 1
/// security vote (one +1/+1 counter on each creature you control).
#[test]
fn issue_888_split_vote_mixes_treasures_and_counters() {
    let (mut runner, emissary, bear) = attack_with_emissary();
    advance_to_vote_prompt(&mut runner);
    cast_votes(&mut runner, ["profit", "security"]);

    assert_eq!(
        treasure_count(&runner, P0),
        2,
        "one profit vote × twice = 2 Treasures"
    );
    assert_eq!(
        plus_counters(&runner, emissary),
        1,
        "one security vote → 1 counter on Emissary Green"
    );
    assert_eq!(
        plus_counters(&runner, bear),
        1,
        "one security vote → 1 counter on the other creature you control"
    );
}

//! Grimdancer's "enters with your choice of two different counters on it from
//! among menace, deathtouch, and lifelink" (issue #7794) — the real cast →
//! resolve → `ChooseOneOfBranch` → `ChooseBranch` pipeline. Choosing two
//! distinct kinds is lowered as one unordered PAIR pick (three branches), and
//! the chosen branch chains two self-targeted `PutCounter`s that both fold
//! onto the entering permanent.
//!
//! REVERT DISCRIMINATOR: without the reordered-list reader the line parses as
//! ONE generic counter literally named "your choice of two different" and no
//! choice is ever offered — the `ChooseOneOfBranch` assertion fails first.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GRIMDANCER: &str = "This creature enters with your choice of two different counters on it from among menace, deathtouch, and lifelink.";

fn counter_count(runner: &GameRunner, object: ObjectId, kind: KeywordKind) -> u32 {
    runner
        .state()
        .objects
        .get(&object)
        .and_then(|card| card.counters.get(&CounterType::Keyword(kind)).copied())
        .unwrap_or(0)
}

fn has_keyword(runner: &GameRunner, object: ObjectId, keyword: &Keyword) -> bool {
    runner
        .state()
        .objects
        .get(&object)
        .is_some_and(|card| card.has_keyword(keyword))
}

/// Cast Grimdancer, expect the three-pair choice, pick `index`, and settle.
fn cast_and_choose(index: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grimdancer = scenario
        .add_creature_to_hand_from_oracle(P0, "Grimdancer", 4, 1, GRIMDANCER)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(grimdancer).resolve();

    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { branches, .. } => assert_eq!(
            branches.len(),
            3,
            "two different from among three kinds must offer the three pairs"
        ),
        other => panic!("expected the pair choice on entry, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("choosing a counter pair must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .objects
            .get(&grimdancer)
            .expect("Grimdancer object exists")
            .zone,
        Zone::Battlefield,
        "Grimdancer must finish entering after the choice"
    );
    (runner, grimdancer)
}

#[test]
fn the_first_pair_folds_menace_and_deathtouch_but_not_lifelink() {
    let (runner, grimdancer) = cast_and_choose(0);

    assert_eq!(counter_count(&runner, grimdancer, KeywordKind::Menace), 1);
    assert_eq!(
        counter_count(&runner, grimdancer, KeywordKind::Deathtouch),
        1
    );
    assert_eq!(
        counter_count(&runner, grimdancer, KeywordKind::Lifelink),
        0,
        "the unchosen kind must not be folded"
    );
    assert!(has_keyword(&runner, grimdancer, &Keyword::Menace));
    assert!(has_keyword(&runner, grimdancer, &Keyword::Deathtouch));
    assert!(
        !has_keyword(&runner, grimdancer, &Keyword::Lifelink),
        "no innate lifelink and no lifelink counter chosen"
    );
}

#[test]
fn the_last_pair_folds_deathtouch_and_lifelink_but_not_menace() {
    let (runner, grimdancer) = cast_and_choose(2);

    assert_eq!(
        counter_count(&runner, grimdancer, KeywordKind::Deathtouch),
        1
    );
    assert_eq!(counter_count(&runner, grimdancer, KeywordKind::Lifelink), 1);
    assert_eq!(counter_count(&runner, grimdancer, KeywordKind::Menace), 0);
    assert!(has_keyword(&runner, grimdancer, &Keyword::Lifelink));
    assert!(
        !has_keyword(&runner, grimdancer, &Keyword::Menace),
        "the unchosen kind must not grant its keyword"
    );
}

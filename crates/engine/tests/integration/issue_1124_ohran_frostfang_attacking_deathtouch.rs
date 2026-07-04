//! Issue #1124 — Ohran Frostfang.
//!
//! "Attacking creatures you control have deathtouch." The static was already
//! parsed correctly (`FilterProp::Attacking { defender: None }` + `AddKeyword`),
//! and the underlying re-evaluation fix shipped for Crossway Troublemakers
//! (`state.layers_dirty.mark_full()` after `declare_attackers`, see
//! `crossway_troublemakers_attacking_keywords.rs`) already covers any card using
//! this pattern. These tests pin Ohran Frostfang specifically: the grant must
//! reach a creature OTHER than the source, and the granted deathtouch must
//! actually destroy a high-toughness blocker via CR 704.5g/CR 702.2c SBA lethal
//! damage, not just appear in `has_keyword`.
//!
//! CR 506.4: A creature is "attacking" from declaration until it leaves combat.
//! CR 613.1f: Layer 6 ability-adding static.
//! CR 702.2c: Deathtouch — any nonzero combat damage is lethal.
//! CR 704.5h: SBA destroys a creature dealt damage by a deathtouch source.
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const OHRAN_FROSTFANG: &str = "Attacking creatures you control have deathtouch.\nWhenever a creature you control deals combat damage to a player, draw a card.";

fn declare_attacker(runner: &mut engine::game::scenario::GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
}

#[test]
fn ohran_frostfang_grants_deathtouch_to_other_attacking_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _ohran = scenario
        .add_creature_from_oracle(P0, "Ohran Frostfang", 2, 6, OHRAN_FROSTFANG)
        .id();
    let other = scenario.add_creature(P0, "Other Creature", 2, 2).id();

    let mut runner = scenario.build();
    declare_attacker(&mut runner, other);

    let obj = &runner.state().objects[&other];
    assert!(
        obj.has_keyword(&Keyword::Deathtouch),
        "Other attacking creature should have deathtouch from Ohran Frostfang"
    );
}

fn declare_and_block(
    scenario: GameScenario,
    attacker: ObjectId,
    blocker: ObjectId,
) -> engine::game::scenario::GameRunner {
    let mut runner = scenario.build();
    declare_attacker(&mut runner, attacker);
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .expect("DeclareBlockers should succeed");
    runner.pass_both_players();
    runner
}

#[test]
fn ohran_frostfang_granted_deathtouch_kills_blocker_with_one_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _ohran = scenario
        .add_creature_from_oracle(P0, "Ohran Frostfang", 2, 6, OHRAN_FROSTFANG)
        .id();
    let attacker = scenario.add_creature(P0, "Other Creature", 1, 1).id();
    let blocker = scenario.add_creature(P1, "Big Blocker", 0, 8).id();

    let runner = declare_and_block(scenario, attacker, blocker);

    assert!(
        !runner.state().battlefield.contains(&blocker),
        "1-power attacker with granted deathtouch should have destroyed the blocker"
    );
}

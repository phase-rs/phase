//! Issue #5263 — Chaos Warp cast-pipeline coverage: shuffle/reveal uses the
//! targeted permanent's owner's library, not the caster's. Behavior already on
//! `main` (#5234 / #2406). Parser AST coverage lives in
//! `issue_2406_chaos_warp_owner_library_shuffle_and_reveal`; resolver-unit
//! coverage lives in `chaos_warp_owner_library.rs`.
//!
//! CR 400.3: A permanent shuffled into a library goes to its owner's library.
//! The reveal step follows the card's wording ("their library" = that owner).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHAOS_WARP_ORACLE: &str = "The owner of target permanent shuffles it into their library, then reveals the top card of their library. If it's a permanent card, they put it onto the battlefield.";

fn put_library_top(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let zone = runner.state().objects.get(&id).expect("object").zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner
            .state_mut()
            .players
            .get_mut(owner.0 as usize)
            .unwrap()
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

#[test]
fn chaos_warp_cast_reveals_target_owner_library_not_caster() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let caster_library_creature = scenario.add_creature(P0, "Caster Top Creature", 1, 1).id();

    let warp = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, warp, false, vec![]),
            ManaUnit::new(ManaType::Colorless, warp, false, vec![]),
            ManaUnit::new(ManaType::Red, warp, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    put_library_top(&mut runner, caster_library_creature);

    runner.cast(warp).target_object(target).resolve();

    assert_eq!(
        runner.state().last_revealed_ids,
        vec![target],
        "Chaos Warp must reveal the top of the targeted permanent owner's library"
    );
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "revealed permanent from the target owner's library must enter the battlefield"
    );
    assert_eq!(
        runner.state().objects[&caster_library_creature].zone,
        Zone::Library,
        "caster's library top must stay in the library when reveal routes to the target owner"
    );
    assert!(
        !runner
            .state()
            .last_revealed_ids
            .contains(&caster_library_creature),
        "Chaos Warp must not reveal from the caster's library"
    );
}

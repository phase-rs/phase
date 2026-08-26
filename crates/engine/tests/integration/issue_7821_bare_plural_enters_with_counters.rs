//! Issue #7821 (Curator Beastie): "Colorless creatures you control enter with
//! two additional +1/+1 counters on them." — the bare-plural distributive
//! subject used to be swallowed, anchoring the replacement to Beastie's own
//! entry. Runtime proof over the real pipeline: a manifested face-down 2/2
//! (colorless, CR 708.2a) enters with the two counters; a colored creature
//! does not; Beastie itself carries none.
//!
//! REVERT DISCRIMINATOR: without the bare-plural subject arm the replacement
//! is SelfRef-anchored — the manifested creature enters bare and the
//! `== 2` assertion fails.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BEASTIE: &str = "Reach\nColorless creatures you control enter with two additional +1/+1 counters on them.\nWhenever this creature enters or attacks, manifest dread.";
const MANIFEST_DREAD: &str = "Manifest dread.";

fn plus_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object)
        .and_then(|card| card.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

#[test]
fn a_manifested_colorless_creature_enters_with_two_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let beastie = scenario
        .add_creature_from_oracle(P0, "Curator Beastie", 4, 5, BEASTIE)
        .id();
    scenario.add_card_to_library_top(P0, "Library Top");
    scenario.add_card_to_library_top(P0, "Second Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    let top = runner.state().players[0].library[0];

    runner.cast(spell).resolve();
    let WaitingFor::ManifestDreadChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "manifest dread must pause for a card choice, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards { cards: vec![top] })
        .expect("manifest choice must be accepted");
    runner.advance_until_stack_empty();

    let manifested = runner
        .state()
        .objects
        .get(&top)
        .expect("manifested object exists");
    assert_eq!(manifested.zone, Zone::Battlefield);
    assert!(manifested.face_down, "manifested object is face down");
    assert_eq!(
        plus_counters(&runner, top),
        2,
        "the colorless face-down 2/2 must enter with Beastie's two counters"
    );
    assert_eq!(
        plus_counters(&runner, beastie),
        0,
        "Beastie (GU) must not receive its own counters"
    );
}

/// Negative + reach-guard pair: a COLORED creature entering under the same
/// Beastie gets nothing (the manifest test above proves the same static DOES
/// reach a colorless entry, so this cannot pass vacuously).
#[test]
fn a_colored_creature_enters_without_beastie_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Curator Beastie", 4, 5, BEASTIE);
    let bear = scenario
        .add_creature_to_hand(P0, "White Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    runner.cast(bear).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&bear).expect("bear exists").zone,
        Zone::Battlefield
    );
    assert_eq!(
        plus_counters(&runner, bear),
        0,
        "a white creature must not match the colorless filter"
    );
}

/// A normally cast colorless creature must also receive the two
/// counters — bisects the runtime path from the manifest-specific one.
#[test]
fn a_cast_colorless_creature_enters_with_two_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Curator Beastie", 4, 5, BEASTIE);
    let golem = scenario
        .add_creature_to_hand(P0, "Plain Golem", 3, 3)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    runner.cast(golem).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .objects
            .get(&golem)
            .expect("golem exists")
            .zone,
        Zone::Battlefield
    );
    assert_eq!(
        plus_counters(&runner, golem),
        2,
        "a cast colorless creature must enter with the two counters"
    );
}

/// The direct guard against the reported self-anchoring symptom: a SECOND
/// Curator Beastie cast through the real entry pipeline (GU — colored) gets
/// nothing from the first one's static. The manifest test proves the same
/// fixture DOES reach a colorless entry, so this cannot pass vacuously.
#[test]
fn a_second_cast_beastie_enters_bare() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Curator Beastie", 4, 5, BEASTIE);
    let second = scenario
        .add_creature_to_hand_from_oracle(P0, "Curator Beastie", 4, 5, BEASTIE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.cast(second).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .objects
            .get(&second)
            .expect("second Beastie exists")
            .zone,
        Zone::Battlefield
    );
    assert_eq!(
        plus_counters(&runner, second),
        0,
        "a cast GU Beastie must not match the colorless filter nor self-anchor"
    );
}

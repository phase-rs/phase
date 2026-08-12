//! Regression for issue #6473: Wrath of the Skies must use the energy actually
//! paid during resolution, rather than the X chosen while casting.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WRATH_OF_THE_SKIES: &str = "You get X {E} (energy counters), then you may pay any amount of {E}. Destroy each artifact, creature, and enchantment with mana value less than or equal to the amount of {E} paid this way.";

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

#[test]
fn wrath_uses_energy_paid_not_announced_x_for_destroy_threshold() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let zero_mana_artifact = scenario
        .add_creature(P1, "Ornithopter", 0, 2)
        .as_artifact()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let thought_knot_seer = scenario
        .add_creature(P1, "Thought-Knot Seer", 4, 4)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of the Skies", false, WRATH_OF_THE_SKIES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::White),
            mana(ManaType::White),
            mana(ManaType::Colorless),
            mana(ManaType::Colorless),
        ],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(wrath).x(2).resolve();

    match outcome.final_waiting_for() {
        WaitingFor::PayAmountChoice { player, max, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(*max, 2, "Wrath must offer the two energy it created");
        }
        other => panic!("expected energy payment choice, got {other:?}"),
    }

    runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("paying zero energy must resume Wrath");

    assert_eq!(runner.state().players[P0.0 as usize].energy, 2);
    assert_eq!(
        runner.state().objects[&zero_mana_artifact].zone,
        Zone::Graveyard,
        "the zero-mana artifact is within the paid-energy threshold"
    );
    assert_eq!(
        runner.state().objects[&thought_knot_seer].zone,
        Zone::Battlefield,
        "the four-mana creature must survive after paying zero energy"
    );
}

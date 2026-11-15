//! Niambi, Beloved Protector — the restriction inside a quoted granted trigger
//! belongs to the granted trigger, through the production pipeline.
//!
//! Oracle text (Scryfall, verbatim):
//! "Flash
//! When Niambi, Beloved Protector enters, if you cast it, choose target
//! nonlegendary creature card in your graveyard that was put there from the
//! battlefield this turn. Return it to the battlefield. It perpetually gains
//! "Whenever this creature becomes the target of a spell or ability, draw a
//! card. This ability triggers only once each turn.""
//!
//! CR 113.1a + CR 603.1: the quoted "This ability triggers only once each
//! turn." is part of the granted trigger's text, so the returned creature draws
//! once for the first time it becomes a target this turn and not again.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const NIAMBI: &str = "Flash\nWhen Niambi, Beloved Protector enters, if you cast it, choose target nonlegendary creature card in your graveyard that was put there from the battlefield this turn. Return it to the battlefield. It perpetually gains \"Whenever this creature becomes the target of a spell or ability, draw a card. This ability triggers only once each turn.\"";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn niambi_granted_draw_trigger_fires_only_once_each_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let niambi = scenario
        .add_creature_to_hand_from_oracle(P0, "Niambi, Beloved Protector", 2, 2, NIAMBI)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 0,
        })
        .as_legendary()
        .id();
    let growths: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                })
                .id()
        })
        .collect();
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
    }
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Red,
            ManaType::White,
            ManaType::Blue,
            ManaType::Green,
            ManaType::Green,
        ]),
    );
    let mut runner = scenario.build();

    // The creature dies this turn, so it is a legal target for Niambi's ETB.
    runner.cast(shock).target_object(bears).resolve();
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);

    runner.cast(niambi).target_object(bears).resolve();
    assert_eq!(runner.state().objects[&niambi].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "Niambi's ETB returns the creature that died this turn"
    );

    let library_size = |runner: &engine::game::scenario::GameRunner| {
        runner.state().players[P0.0 as usize].library.len()
    };
    let before = library_size(&runner);

    let outcome = runner.cast(growths[0]).target_object(bears).resolve();
    assert!(outcome.state().stack.is_empty());
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the first targeting spell and granted draw finish resolving: {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        library_size(&runner),
        before - 1,
        "the first time the creature becomes a target this turn, it draws"
    );

    let outcome = runner.cast(growths[1]).target_object(bears).resolve();
    assert!(outcome.state().stack.is_empty());
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the second targeting spell finishes resolving: {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        library_size(&runner),
        before - 1,
        "CR 603.1: the granted trigger triggers only once each turn"
    );
}

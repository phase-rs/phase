//! Runtime regression for issue #4779: a pure "do the same for <type>" clause
//! must repeat the preceding mass zone-change for the sibling card type.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RETURN_CREATURE_PARTITIONS: &str =
    "Return all non-Human creature cards from your graveyard to the battlefield, then do the same for Human cards.";

/// CR 608.2c: the Human continuation repeats the return instruction after the
/// non-Human creatures. Both partitions use attachment-free creature cards so
/// the test isolates repetition rather than Aura attachment legality.
#[test]
fn do_the_same_for_type_returns_both_enchantment_partitions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Partition Return", false, RETURN_CREATURE_PARTITIONS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let non_human = scenario
        .add_creature_from_oracle(P0, "Non-Human Creature", 0, 1, "")
        .id();
    let human = scenario
        .add_creature_from_oracle(P0, "Human Creature", 0, 1, "")
        .with_subtypes(vec!["Human"])
        .id();

    let mut runner = scenario.build();
    let mut setup_events = Vec::new();
    for object_id in [non_human, human] {
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            object_id,
            Zone::Graveyard,
            &mut setup_events,
        );
    }

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the parsed return spell");
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("advance the spell through normal resolution");
    }

    assert!(
        runner.state().stack.is_empty(),
        "return spell must finish resolving"
    );
    assert_eq!(runner.state().objects[&non_human].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&human].zone,
        Zone::Battlefield,
        "the Human must be returned by the repeated type-substitution instruction"
    );
}

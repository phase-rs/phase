//! Issue #4834: For the Ancestors must allow revealing cards of the chosen type.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FOR_THE_ANCESTORS: &str = "Choose a creature type. Look at the top six cards of your library. You may reveal any number of cards of the chosen type from among them and put the revealed cards into your hand. Put the rest on the bottom of your library in a random order.\nFlashback {3}{G}";

fn stage_creature(runner: &mut engine::game::scenario::GameRunner, id: engine::types::identifiers::ObjectId, subtype: &str) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes = vec![subtype.to_string()];
    obj.base_card_types = obj.card_types.clone();
}

#[test]
fn for_the_ancestors_exposes_matching_cards_in_dig_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let elf_one = scenario.add_card_to_library_top(P0, "Elf One");
    let non_elf = scenario.add_card_to_library_top(P0, "Non-Elf");
    let elf_two = scenario.add_card_to_library_top(P0, "Elf Two");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "For the Ancestors", false, FOR_THE_ANCESTORS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    stage_creature(&mut runner, elf_one, "Elf");
    stage_creature(&mut runner, elf_two, "Elf");
    stage_creature(&mut runner, non_elf, "Human");
    runner
        .state_mut()
        .players[P0.0 as usize]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Green,
            spell,
            false,
            vec![],
        ));

    runner.cast(spell).resolve();
    runner
        .act(GameAction::ChooseOption {
            choice: "Elf".to_string(),
        })
        .expect("choose Elf");

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice {
                selectable_cards,
                keep_count,
                ..
            } => {
                assert!(
                    !selectable_cards.is_empty(),
                    "matching Elf cards must be selectable"
                );
                assert!(keep_count > 0, "keep_count must reflect selectable Elves");
                return;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("stack emptied without presenting DigChoice");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    panic!("never reached DigChoice prompt");
}

#[test]
fn for_the_ancestors_puts_revealed_elves_in_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let elf_one = scenario.add_card_to_library_top(P0, "Elf One");
    let non_elf = scenario.add_card_to_library_top(P0, "Non-Elf");
    let elf_two = scenario.add_card_to_library_top(P0, "Elf Two");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "For the Ancestors", false, FOR_THE_ANCESTORS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    stage_creature(&mut runner, elf_one, "Elf");
    stage_creature(&mut runner, elf_two, "Elf");
    stage_creature(&mut runner, non_elf, "Human");
    runner
        .state_mut()
        .players[P0.0 as usize]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Green,
            spell,
            false,
            vec![],
        ));

    runner.cast(spell).resolve();
    runner
        .act(GameAction::ChooseOption {
            choice: "Elf".to_string(),
        })
        .expect("choose Elf");

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                let kept: Vec<_> = cards
                    .iter()
                    .filter(|id| {
                        runner.state().objects[id]
                            .card_types
                            .subtypes
                            .iter()
                            .any(|s| s == "Elf")
                    })
                    .copied()
                    .collect();
                runner
                    .act(GameAction::SelectCards { cards: kept })
                    .expect("keep all Elves");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    let zone_of = |id| runner.state().objects[&id].zone;
    assert_eq!(zone_of(elf_one), Zone::Hand);
    assert_eq!(zone_of(elf_two), Zone::Hand);
    assert_eq!(zone_of(non_elf), Zone::Library);
}

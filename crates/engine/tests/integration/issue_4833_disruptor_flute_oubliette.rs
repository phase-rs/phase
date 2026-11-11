//! Issue #4833: Disruptor Flute commander tax and Oubliette host-bound phasing.

use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use std::sync::Arc;

const DISRUPTOR_FLUTE: &str =
    "Flash\nAs this artifact enters, choose a card name.\nSpells with the chosen name cost {3} more to cast.\nActivated abilities of sources with the chosen name can't be activated unless they're mana abilities.";

const OUBLIETTE: &str = "When this enchantment enters, target creature phases out until this enchantment leaves the battlefield. Tap that creature as it phases in this way.";

#[test]
fn disruptor_flute_raises_cost_of_named_commander() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let commander_name = "Atraxa, Praetors' Voice";
    let commander = scenario
        .add_creature_from_oracle(P0, commander_name, 4, 4, "")
        .id();
    scenario.with_commander(commander);

    let flute = scenario
        .add_creature_to_hand_from_oracle(P0, "Disruptor Flute", 0, 0, DISRUPTOR_FLUTE)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    runner.state_mut().all_card_names = Arc::from([commander_name.to_string()]);

    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Colorless,
            flute,
            false,
            vec![],
        ));
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Colorless,
            flute,
            false,
            vec![],
        ));
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Colorless,
            flute,
            false,
            vec![],
        ));

    runner.cast(flute).resolve();

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: commander_name.to_string(),
                    })
                    .expect("choose commander name");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    let base = runner.state().objects[&commander].mana_cost.mana_value();
    let taxed = display_spell_cost(runner.state(), P0, commander).expect("display cost");
    assert_eq!(
        taxed.mana_value(),
        base + 3,
        "Disruptor Flute must add {{3}} to the named commander's cast cost"
    );
}

#[test]
fn oubliette_keeps_creature_phased_out_until_it_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    let attached_equipment = scenario
        .add_creature(P1, "Test Blade", 0, 1)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text("Equipped creature gets +1/+1.\nEquip {0}")
        .id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let oubliette = scenario
        .add_creature_to_hand_from_oracle(P0, "Oubliette", 0, 0, OUBLIETTE)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                engine::types::mana::ManaCostShard::Black,
                engine::types::mana::ManaCostShard::Black,
            ],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    // Equip is sorcery-speed and the creature is P1's, so borrow P1's turn
    // just for the production attach path, then restore P0's precombat main.
    let saved_active = runner.state().active_player;
    let saved_priority = runner.state().priority_player;
    let saved_phase = runner.state().phase;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .activate(attached_equipment, 0)
        .target_object(creature)
        .resolve();
    runner.state_mut().active_player = saved_active;
    runner.state_mut().priority_player = saved_priority;
    runner.state_mut().phase = saved_phase;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    assert_eq!(
        runner.state().objects[&attached_equipment].attached_to,
        Some(creature.into()),
        "the equipment must be genuinely attached before Oubliette phases the host out"
    );
    for _ in 0..3 {
        runner.state_mut().players[P0.0 as usize].mana_pool.add(
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                oubliette,
                false,
                vec![],
            ),
        );
    }
    for _ in 0..2 {
        runner.state_mut().players[P0.0 as usize].mana_pool.add(
            engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Black,
                oubliette,
                false,
                vec![],
            ),
        );
    }

    runner.cast(oubliette).target_object(creature).resolve();

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(creature)),
                    })
                    .expect("target creature");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    assert!(
        runner.state().objects[&creature].is_phased_out(),
        "Oubliette must phase out the targeted creature on entry"
    );
    assert!(
        runner.state().objects[&attached_equipment].is_phased_out(),
        "reach guard: the attachment must phase out indirectly with its host"
    );

    runner.advance_to_end_step();

    assert!(
        runner.state().objects[&creature].is_phased_out(),
        "creature must stay phased out while Oubliette remains"
    );

    runner
        .act(GameAction::Debug(DebugAction::Sacrifice {
            object_id: oubliette,
        }))
        .expect("sacrifice Oubliette");

    for _ in 0..48 {
        let phased_out = runner.state().objects[&creature].is_phased_out();
        if !phased_out
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    let creature_obj = &runner.state().objects[&creature];
    assert!(
        !creature_obj.is_phased_out(),
        "creature must phase back in when Oubliette leaves"
    );
    assert!(
        creature_obj.tapped,
        "creature must enter tapped as it phases in this way"
    );
    let attachment = &runner.state().objects[&attached_equipment];
    assert!(
        attachment.is_phased_in(),
        "an attachment phased out with its host must phase in with it"
    );
    assert!(
        !attachment.tapped,
        "the tap rider names only 'that creature', not an attached permanent"
    );
}

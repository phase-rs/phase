//! Complete relative populations at their ordinary modal spell consumer.
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COLLISION: &str = "Choose one —\n• Collision Course deals X damage to target creature, where X is the number of permanents you control that are creatures and/or Vehicles.\n• Destroy target artifact.";

/// CR 601.2b + CR 601.2c + CR 601.2h: both paid modes bind their own
/// announced target. CR 205.2b + CR 608.2h: count distinct live permanents.
#[test]
fn collision_both_paid_modes_keep_whole_population_and_announced_target() {
    for caster in [P0, P1] {
        let other = if caster == P0 { P1 } else { P0 };
        for mode in [0, 1] {
            for change in [0, 1, 2, 3] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let spell = scenario
                    .add_spell_to_hand_from_oracle(caster, "Collision Course", false, COLLISION)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::White],
                    })
                    .id();
                let bounce = scenario
                    .add_spell_to_hand_from_oracle(
                        caster,
                        "Disperse",
                        true,
                        "Return target nonland permanent to its owner's hand.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                let ordinary = scenario
                    .add_creature(other, "Ordinary creature", 1, 20)
                    .controlled_by(caster)
                    .id();
                let control = scenario
                    .add_spell_to_hand_from_oracle(
                        other,
                        "Synthetic Vehicle control",
                        true,
                        "Gain control of target artifact.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                scenario.with_mana_pool(
                    other,
                    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
                );
                let vehicle = scenario
                    .add_artifact_from_oracle(caster, "Noncreature Vehicle", "")
                    .with_subtypes(vec!["Vehicle"])
                    .id();
                let overlap = scenario
                    .add_creature(caster, "Creature Vehicle", 1, 20)
                    .as_artifact_creature()
                    .with_subtypes(vec!["Vehicle"])
                    .id();
                scenario.add_enchantment_from_oracle(caster, "Nonmatching own permanent", "");
                let other_creature = scenario
                    .add_creature(other, "First creature option", 1, 20)
                    .id();
                let selected_creature = scenario
                    .add_creature(other, "Second creature option", 1, 20)
                    .id();
                let other_artifact = scenario
                    .add_artifact_from_oracle(other, "First artifact option", "")
                    .id();
                let selected_artifact = scenario
                    .add_artifact_from_oracle(other, "Second artifact option", "")
                    .id();
                let foreign = scenario
                    .add_artifact_from_oracle(caster, "Foreign Vehicle", "")
                    .with_subtypes(vec!["Vehicle"])
                    .controlled_by(other)
                    .id();
                scenario.with_mana_pool(
                    caster,
                    [
                        ManaType::White,
                        ManaType::Colorless,
                        ManaType::Blue,
                        ManaType::Colorless,
                    ]
                    .into_iter()
                    .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
                    .collect(),
                );
                let mut runner = scenario.build();
                runner.state_mut().active_player = caster;
                runner.state_mut().priority_player = caster;
                runner.state_mut().waiting_for = WaitingFor::Priority { player: caster };
                let card_id = runner.state().objects[&spell].card_id;
                runner
                    .act(GameAction::CastSpell {
                        object_id: spell,
                        card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .unwrap();
                let WaitingFor::ModeChoice {
                    player,
                    modal,
                    pending_cast,
                    unavailable_modes,
                } = &runner.state().waiting_for
                else {
                    panic!("actual Collision mode announcement")
                };
                assert_eq!(*player, caster);
                assert_eq!(
                    (modal.min_choices, modal.max_choices, modal.mode_count),
                    (1, 1, 2)
                );
                assert_eq!(pending_cast.object_id, spell);
                assert!(unavailable_modes.is_empty());
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![mode],
                    })
                    .unwrap();
                let WaitingFor::TargetSelection {
                    player,
                    pending_cast,
                    target_slots,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("actual selected-mode targeting")
                };
                assert_eq!(*player, caster);
                assert_eq!(pending_cast.object_id, spell);
                assert_eq!(target_slots.len(), 1);
                let target = if mode == 0 {
                    selected_creature
                } else {
                    selected_artifact
                };
                let mut expected_options = if mode == 0 {
                    vec![ordinary, overlap, other_creature, selected_creature]
                } else {
                    vec![vehicle, overlap, other_artifact, selected_artifact]
                };
                if mode == 1 {
                    expected_options.push(foreign);
                }
                let mut options = target_slots[0]
                    .legal_targets
                    .iter()
                    .map(|target| match target {
                        TargetRef::Object(id) => *id,
                        _ => panic!("object-only mode"),
                    })
                    .collect::<Vec<_>>();
                options.sort();
                expected_options.sort();
                assert_eq!(options, expected_options);
                assert_ne!(
                    target_slots[0].legal_targets.first(),
                    Some(&TargetRef::Object(target))
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .unwrap();
                let entry = runner.state().stack.last().unwrap();
                assert_eq!(
                    (entry.id, entry.source_id, entry.controller),
                    (spell, spell, caster)
                );
                assert_eq!(
                    entry.ability().unwrap().targets,
                    vec![TargetRef::Object(target)]
                );
                assert!(matches!(
                    entry.kind,
                    StackEntryKind::Spell {
                        actual_mana_spent: 2,
                        ..
                    }
                ));
                assert_eq!(
                    runner.state().players[caster.0 as usize].mana_pool.total(),
                    2
                );
                if change == 3 {
                    runner.act(GameAction::PassPriority).unwrap();
                    assert_eq!(runner.state().priority_player, other);
                    runner.cast(control).target_object(vehicle).commit();
                    runner.resolve_top();
                    assert_eq!(runner.state().objects[&vehicle].zone, Zone::Battlefield);
                    assert_eq!(runner.state().objects[&vehicle].controller, other);
                    assert_eq!(runner.state().objects[&vehicle].owner, caster);
                } else if change != 0 {
                    // Public paid bounce changes the population or removes the selected target.
                    let selected = if change == 1 { vehicle } else { target };
                    runner.cast(bounce).target_object(selected).commit();
                    runner.resolve_top();
                    assert_eq!(runner.state().objects[&selected].zone, Zone::Hand);
                }
                let raw = serde_json::to_value(runner.state()).unwrap();
                let current: engine::types::game_state::GameState =
                    serde_json::from_value(raw.clone()).unwrap();
                let trusted =
                    engine::types::game_state::PersistedGameState::capture(runner.state().clone());
                let mut legacy = raw;
                legacy.as_object_mut().unwrap().remove("resolution_stack");
                legacy["resolution_state_version"] = serde_json::json!(1);
                let legacy =
                    serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
                        legacy,
                    )
                    .unwrap()
                    .into_game_state();
                for restored in [current, trusted.into_game_state().unwrap(), legacy] {
                    let mut runner = engine::game::scenario::GameRunner::from_state(restored);
                    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
                    runner.advance_until_stack_empty();
                    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                    if mode == 0 {
                        assert_eq!(
                            runner.state().objects[&selected_creature].damage_marked,
                            if change == 2 {
                                0
                            } else if change == 1 || change == 3 {
                                2
                            } else {
                                3
                            }
                        );
                        assert_eq!(runner.state().objects[&other_creature].damage_marked, 0);
                    } else {
                        assert_eq!(
                            runner.state().objects[&selected_artifact].zone,
                            if change == 2 {
                                Zone::Hand
                            } else {
                                Zone::Graveyard
                            }
                        );
                        assert_eq!(
                            runner.state().objects[&other_artifact].zone,
                            Zone::Battlefield
                        );
                    }
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        runner.state().stack.is_empty()
                            && runner.state().trigger_stack.is_empty()
                            && runner.state().resolution_stack.is_empty()
                    );
                }
            }
        }
    }
}

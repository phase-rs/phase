//! Printed target occurrences and event-relative player declarations.
//! The fixture text is the unchanged 2026-10-04 AtomicCards text.
//!
//! CR 601.2b-c and CR 601.2h govern intentions, independent targets and full
//! payment; CR 608.2b-c preserves written damage assignments after illegality.
//! CR 603.3d governs Lord's announced trigger recipient; CR 109.5 and CR 113.8
//! keep event caster and ability controller distinct. CR 202.3e uses announced
//! X in the event spell's mana value. CR 702.111b supplies independent menace.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const MAGMA: &str = "Kicker—Sacrifice two lands. (You may sacrifice two lands in addition to any other costs as you cast this spell.)\nMagma Burst deals 3 damage to any target. If this spell was kicked, it deals 3 damage to another target.";
const COMET: &str = "Multikicker {1} (You may pay an additional {1} any number of times as you cast this spell.)\nChoose any target, then choose another target for each time this spell was kicked. Comet Storm deals X damage to each of them.";
const CONE: &str = "Cone of Flame deals 1 damage to any target, 2 damage to another target, and 3 damage to a third target.";
const LORD: &str = "Menace\nYour opponents can't gain life.\nWhenever a player casts their first spell each turn, choose another target player. The Lord of Pain deals damage equal to that spell's mana value to the chosen player.";

#[test]
fn kaima_end_step_checks_opponent_creatures_and_aura_controller_at_resolution() {
    use engine::game::effects::attach::attach_to;
    let database = super::search_result_conditional_destination::load_finite_printed_records(
        EVENT_TARGET_COHORT_PRINTED,
    );
    let face = database
        .get_face_by_name("Kaima, the Fractured Calm")
        .unwrap();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let kaima = scenario.add_creature(P0, &face.name, 3, 3).id();
    let marked = scenario.add_creature(P1, "Marked opponent", 2, 2).id();
    let late = scenario
        .add_creature(P1, "Aura moved here before resolution", 2, 2)
        .id();
    let foreign_aura_host = scenario.add_creature(P1, "Opponent Aura only", 2, 2).id();
    let equipment_host = scenario.add_creature(P1, "Equipment only", 2, 2).id();
    let unmarked = scenario.add_creature(P1, "No attachment", 2, 2).id();
    let own = scenario.add_creature(P0, "Own marked creature", 2, 2).id();
    let own_aura = scenario
        .add_enchantment_from_oracle(P0, "Own Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let moving_aura = scenario
        .add_enchantment_from_oracle(P0, "Moving Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let opponent_aura = scenario
        .add_enchantment_from_oracle(P1, "Foreign Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let own_host_aura = scenario
        .add_enchantment_from_oracle(P0, "Own host Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Own Equipment", "Equip {1}")
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    engine::game::printed_cards::apply_card_face_to_object(
        runner.state_mut().objects.get_mut(&kaima).unwrap(),
        face,
    );
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), kaima);
    for (attachment, host) in [
        (own_aura, marked),
        (moving_aura, marked),
        (opponent_aura, foreign_aura_host),
        (own_host_aura, own),
        (equipment, equipment_host),
    ] {
        attach_to(runner.state_mut(), attachment, host);
        assert_eq!(
            runner.state().objects[&attachment].attached_to,
            Some(engine::game::game_object::AttachTarget::Object(host))
        );
    }
    for _ in 0..16 {
        if !runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().phase, Phase::End);
    assert!(
        !runner.state().stack.is_empty(),
        "the real end-step trigger must be pending"
    );
    // CR 608.2h + CR 303.4e: eligibility is live until this instruction resolves.
    attach_to(runner.state_mut(), moving_aura, late);
    assert_eq!(
        runner.state().objects[&moving_aura].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(late))
    );
    complete(&mut runner);
    assert!(runner.state().objects[&marked].goaded_by.contains(&P0));
    assert!(runner.state().objects[&late].goaded_by.contains(&P0));
    for id in [foreign_aura_host, equipment_host, unmarked, own, kaima] {
        assert!(
            !runner.state().objects[&id].goaded_by.contains(&P0),
            "recipient and Aura controller restrictions: {id:?}"
        );
    }
    assert_eq!(
        runner.state().objects[&kaima]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1),
        Some(&2),
        "the existing GoadAll publisher must supply exactly the creatures goaded this way"
    );
    // CR 701.15a: losing the attachment after resolution does not undo goad.
    move_to_zone(runner.state_mut(), own_aura, Zone::Graveyard, &mut vec![]);
    assert!(runner.state().objects[&marked].goaded_by.contains(&P0));
}

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

fn pool(generic: usize, red: usize) -> Vec<ManaUnit> {
    std::iter::repeat_n(ManaType::Colorless, generic)
        .chain(std::iter::repeat_n(ManaType::Red, red))
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn begin(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("ordinary paid spell announcement");
}

fn choose(runner: &mut GameRunner, target: TargetRef) {
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "required announced target prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(target_slots[selection.current_slot]
        .legal_targets
        .contains(&target));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(target),
        })
        .expect("legal nondefault target");
}

fn optional(runner: &mut GameRunner, times: u32, pay: bool) {
    let WaitingFor::OptionalCostChoice {
        player,
        times_kicked,
        ..
    } = runner.state().waiting_for
    else {
        panic!(
            "required kicker intention before targets: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(times_kicked, times);
    runner
        .act(GameAction::DecideOptionalCost { pay })
        .expect("declare kicker intention");
}

fn complete(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

#[test]
fn magma_real_two_land_kicker_announces_two_distinct_targets_before_sacrificing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands: Vec<_> = (0..3)
        .map(|_| {
            scenario
                .add_land_from_oracle(P0, "Land witness", "{T}: Add {R}.")
                .id()
        })
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Magma Burst", true, MAGMA)
        .with_mana_cost(cost(3, vec![ManaCostShard::Red]))
        .id();
    scenario.with_mana_pool(P0, pool(3, 1));
    let mut runner = scenario.build();
    begin(&mut runner, spell);
    // CR 601.2b-c: the second occurrence exists because kicker was announced.
    optional(&mut runner, 0, true);
    assert!(lands
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Battlefield));
    choose(&mut runner, TargetRef::Player(P1));
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1))
        })
        .is_err());
    choose(&mut runner, TargetRef::Player(P0));
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "real two-land payment must follow target declaration: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 2);
    assert!(lands.iter().all(|id| choices.contains(id)));
    runner
        .act(GameAction::SelectCards {
            cards: vec![lands[2], lands[1]],
        })
        .expect("pay nondefault two-land cost");
    assert_eq!(runner.state().objects[&lands[0]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&lands[1]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&lands[2]].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(runner.state().players[1].life, 17);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn magma_unkicked_one_target_and_unpayable_kicker_preserve_lands() {
    for land_count in [1, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let lands: Vec<_> = (0..land_count)
            .map(|_| {
                scenario
                    .add_land_from_oracle(P0, "Land witness", "{T}: Add {R}.")
                    .id()
            })
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Magma Burst", true, MAGMA)
            .with_mana_cost(cost(3, vec![ManaCostShard::Red]))
            .id();
        scenario.with_mana_pool(P0, pool(3, 1));
        let mut runner = scenario.build();
        begin(&mut runner, spell);
        if land_count == 2 {
            optional(&mut runner, 0, false);
        }
        let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
            panic!("unkicked target prompt");
        };
        assert_eq!(target_slots.len(), 1);
        choose(&mut runner, TargetRef::Player(P1));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        complete(&mut runner);
        assert_eq!(runner.state().players[1].life, 17);
        assert_eq!(runner.state().players[0].life, 20);
        assert!(lands
            .iter()
            .all(|id| runner.state().objects[id].zone == Zone::Battlefield));
    }
}

#[test]
fn comet_x_four_two_kicks_pays_six_rr_and_hits_three_distinct_recipients() {
    for invalid in [None, Some(1), Some(3)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victims: Vec<_> = (0..3)
            .map(|_| scenario.add_creature(P1, "Ten toughness", 1, 10).id())
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Comet Storm", true, COMET)
            .with_mana_cost(cost(
                0,
                vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            ))
            .id();
        scenario.with_mana_pool(P0, pool(6, 2));
        let mut runner = scenario.build();
        begin(&mut runner, spell);
        optional(&mut runner, 0, true);
        optional(&mut runner, 1, true);
        optional(&mut runner, 2, false);
        let WaitingFor::ChooseXValue { player, .. } = runner.state().waiting_for else {
            panic!("announce X independently");
        };
        assert_eq!(player, P0);
        runner
            .act(GameAction::ChooseX { value: 4 })
            .expect("announce X4");
        choose(&mut runner, TargetRef::Object(victims[2]));
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victims[2]))
            })
            .is_err());
        choose(&mut runner, TargetRef::Object(victims[0]));
        choose(&mut runner, TargetRef::Object(victims[1]));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        // CR 608.2b: late illegality neither changes X nor redistributes damage.
        if let Some(number) = invalid {
            for id in victims.iter().take(number) {
                move_to_zone(runner.state_mut(), *id, Zone::Graveyard, &mut vec![]);
            }
        }
        if invalid == Some(3) {
            let mut events = vec![];
            for _ in 0..32 {
                if runner.state().stack.is_empty() {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("ordinary all-invalid paid Comet retirement")
                        .events,
                );
            }
            assert!(runner.state().stack.is_empty());
            assert!(events.iter().all(|event| !matches!(event, engine::types::events::GameEvent::DamageDealt { source_id, .. } if *source_id == spell)));
        }
        complete(&mut runner);
        for (index, id) in victims.iter().enumerate() {
            if invalid.is_some_and(|number| index < number) {
                assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
            } else {
                assert_eq!(runner.state().objects[id].damage_marked, 4);
            }
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    }
}

#[test]
fn comet_zero_kicks_still_uses_announced_x_with_one_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Comet Storm", true, COMET)
        .with_mana_cost(cost(
            0,
            vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
        ))
        .id();
    scenario.with_mana_pool(P0, pool(4, 2));
    let mut runner = scenario.build();
    begin(&mut runner, spell);
    optional(&mut runner, 0, false);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { player: P0, .. }
    ));
    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
    choose(&mut runner, TargetRef::Player(P1));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    complete(&mut runner);
    assert_eq!(runner.state().players[1].life, 16);
}

#[test]
fn comet_two_kicks_cannot_underpay_x_four_but_exact_x_three_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let third = scenario
        .add_creature(P1, "Third Comet recipient", 1, 10)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Comet Storm", true, COMET)
        .with_mana_cost(cost(
            0,
            vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
        ))
        .id();
    scenario.with_mana_pool(P0, pool(5, 2));
    let mut runner = scenario.build();
    begin(&mut runner, spell);
    optional(&mut runner, 0, true);
    optional(&mut runner, 1, true);
    optional(&mut runner, 2, false);
    let WaitingFor::ChooseXValue { player, max, .. } = runner.state().waiting_for else {
        panic!("X after latched multikicker intentions");
    };
    assert_eq!(player, P0);
    assert_eq!(max, 3);
    assert!(runner.act(GameAction::ChooseX { value: 4 }).is_err());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    runner
        .act(GameAction::ChooseX { value: 3 })
        .expect("affordable nondefault X after rejected underpayment");
    choose(&mut runner, TargetRef::Player(P1));
    choose(&mut runner, TargetRef::Object(third));
    choose(&mut runner, TargetRef::Player(P0));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(runner.state().players[1].life, 17);
    assert_eq!(runner.state().objects[&third].damage_marked, 3);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn cone_written_one_two_three_survive_holes_without_compaction() {
    for removed in [vec![], vec![0], vec![1], vec![0, 1, 2]] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victims: Vec<_> = (0..3)
            .map(|_| scenario.add_creature(P1, "Ten toughness", 1, 10).id())
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Cone of Flame", false, CONE)
            .with_mana_cost(cost(3, vec![ManaCostShard::Red, ManaCostShard::Red]))
            .id();
        let response = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Late legality response witness",
                true,
                if removed.len() == 3 {
                    "Destroy all creatures."
                } else {
                    "Destroy target creature."
                },
            )
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(P0, pool(3, 2));
        scenario.with_mana_pool(P1, pool(1, 0));
        let mut runner = scenario.build();
        begin(&mut runner, spell);
        for (index, id) in victims.iter().enumerate() {
            if index > 0 {
                assert!(runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victims[0]))
                    })
                    .is_err());
            }
            choose(&mut runner, TargetRef::Object(*id));
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        if !removed.is_empty() {
            runner.state_mut().priority_player = P1;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
            if removed.len() == 3 {
                let outcome = runner.cast(response).resolve();
                assert!(outcome.events().iter().all(|event| !matches!(event, engine::types::events::GameEvent::DamageDealt { source_id, .. } if *source_id == spell)));
            } else {
                runner
                    .cast(response)
                    .target_object(victims[removed[0]])
                    .resolve();
            }
            assert_eq!(runner.state().objects[&response].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[1].mana_pool.total(), 0);
        }
        complete(&mut runner);
        // CR 608.2b-c: written occurrence addresses retain their own amounts.
        for (index, id) in victims.iter().enumerate() {
            if removed.contains(&index) {
                assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
            } else {
                assert_eq!(runner.state().objects[id].damage_marked, index as u32 + 1);
            }
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    }
}

#[test]
fn cone_rejects_two_available_targets_beside_paid_three_target_positive() {
    for enough in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let third = enough.then(|| {
            scenario
                .add_creature(P0, "Third legal recipient", 1, 10)
                .id()
        });
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Cone of Flame", false, CONE)
            .with_mana_cost(cost(3, vec![ManaCostShard::Red, ManaCostShard::Red]))
            .id();
        scenario.with_mana_pool(P0, pool(3, 2));
        let mut runner = scenario.build();
        if let Some(third) = third {
            begin(&mut runner, spell);
            choose(&mut runner, TargetRef::Player(P1));
            choose(&mut runner, TargetRef::Player(P0));
            choose(&mut runner, TargetRef::Object(third));
            complete(&mut runner);
            assert_eq!(runner.state().players[1].life, 19);
            assert_eq!(runner.state().players[0].life, 18);
            assert_eq!(runner.state().objects[&third].damage_marked, 3);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        } else {
            let card_id = runner.state().objects[&spell].card_id;
            assert!(runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto
                })
                .is_err());
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
            assert_eq!(runner.state().players[0].mana_pool.total(), 5);
            assert!(runner.state().stack.is_empty());
        }
    }
}

#[test]
fn magma_paid_kicker_late_first_illegality_keeps_second_three_and_all_invalid_fizzles() {
    for all in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario
            .add_creature(P1, "First Magma recipient", 1, 10)
            .id();
        let second = scenario
            .add_creature(P1, "Second Magma recipient", 1, 10)
            .id();
        let lands: Vec<_> = (0..2)
            .map(|_| {
                scenario
                    .add_land_from_oracle(P0, "Land witness", "{T}: Add {R}.")
                    .id()
            })
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Magma Burst", true, MAGMA)
            .with_mana_cost(cost(3, vec![ManaCostShard::Red]))
            .id();
        let response = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Magma legality response witness",
                true,
                if all {
                    "Destroy all creatures."
                } else {
                    "Destroy target creature."
                },
            )
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(P0, pool(3, 1));
        scenario.with_mana_pool(P1, pool(1, 0));
        let mut runner = scenario.build();
        runner
            .cast(spell)
            .accept_optional()
            .target_objects(&[first, second])
            .sacrifice_with(&lands)
            .commit();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert!(lands
            .iter()
            .all(|land| runner.state().objects[land].zone == Zone::Graveyard));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        if all {
            let outcome = runner.cast(response).resolve();
            assert!(outcome.events().iter().all(|event| !matches!(event, engine::types::events::GameEvent::DamageDealt { source_id, .. } if *source_id == spell)));
        } else {
            runner.cast(response).target_object(first).resolve();
        }
        complete(&mut runner);
        assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
        if all {
            assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
        } else {
            assert_eq!(runner.state().objects[&second].damage_marked, 3);
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    }
}

fn lord_target(runner: &mut GameRunner, caster: PlayerId, recipient: PlayerId) {
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "first actual cast must request Lord's local recipient: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let legal = &target_slots[selection.current_slot].legal_targets;
    assert!(!legal.contains(&TargetRef::Player(caster)));
    assert!(legal.contains(&TargetRef::Player(recipient)));
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(caster))
        })
        .is_err());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(recipient)),
        })
        .expect("nondefault legal local player");
}

#[test]
fn lord_opponents_first_spell_targets_you_and_second_spell_does_not_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_from_oracle(P0, "The Lord of Pain", 5, 5, LORD)
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(P1, "First spell witness", true, "You gain 3 life.")
        .with_mana_cost(cost(2, vec![ManaCostShard::Red]))
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P1, "Second spell witness", true, "You gain 3 life.")
        .with_mana_cost(cost(2, vec![ManaCostShard::Red]))
        .id();
    scenario.with_mana_pool(P1, pool(4, 2));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&lord)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::Player(P0));
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    begin(&mut runner, first);
    lord_target(&mut runner, P1, P0);
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(
        runner.state().players[1].life,
        20,
        "independent life-gain prohibition"
    );
    assert!(runner.state().objects[&lord]
        .keywords
        .contains(&engine::types::keywords::Keyword::Menace));
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    begin(&mut runner, second);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id != lord));
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
}

#[test]
fn lord_local_recipient_beats_persisted_choice_and_x_survives_spell_departure() {
    let mut scenario = GameScenario::new_n_player(3, 19);
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_from_oracle(P2, "The Lord of Pain", 5, 5, LORD)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "X spell witness", true, "You gain X life.")
        .with_mana_cost(cost(0, vec![ManaCostShard::X, ManaCostShard::Red]))
        .id();
    scenario.with_mana_pool(P1, pool(4, 1));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&lord)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::Player(P0));
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    begin(&mut runner, spell);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { player: P1, .. }
    ));
    runner
        .act(GameAction::ChooseX { value: 4 })
        .expect("event spell announces X4");
    lord_target(&mut runner, P1, P2);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
    // CR 202.3e and CR 608.2h preserve the departed spell's stack mana value;
    // CR 113.7a lets the Lord's already triggered ability survive its source.
    move_to_zone(runner.state_mut(), spell, Zone::Exile, &mut vec![]);
    move_to_zone(runner.state_mut(), lord, Zone::Graveyard, &mut vec![]);
    complete(&mut runner);
    assert_eq!(runner.state().players[2].life, 15);
    assert_eq!(runner.state().players[0].life, 20);
    assert_eq!(runner.state().players[1].life, 20);
}

#[test]
fn lord_pending_first_spells_keep_each_event_caster_mana_value_and_local_recipient() {
    let mut scenario = GameScenario::new_n_player(3, 23);
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_from_oracle(P2, "The Lord of Pain", 5, 5, LORD)
        .controlled_by(P0)
        .id();
    let first = scenario
        .add_spell_to_hand_from_oracle(P1, "First caster event witness", true, "You gain 0 life.")
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P2, "Second caster event witness", true, "You gain 0 life.")
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(P1, pool(2, 0));
    scenario.with_mana_pool(P2, pool(3, 0));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&lord)
        .unwrap()
        .chosen_attributes
        .push(engine::types::ability::ChosenAttribute::Player(P1));
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    begin(&mut runner, first);
    lord_target(&mut runner, P1, P2);
    assert_eq!(runner.state().objects[&first].zone, Zone::Stack);
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
    begin(&mut runner, second);
    lord_target(&mut runner, P2, P0);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == lord)
            .count(),
        2
    );
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
    assert_eq!(runner.state().players[2].mana_pool.total(), 0);
    move_to_zone(runner.state_mut(), lord, Zone::Graveyard, &mut vec![]);
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(runner.state().players[2].life, 18);
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
}

#[test]
fn lord_each_casters_first_spell_includes_its_controller_and_resets_on_real_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Lord of Pain", 5, 5, LORD);
    let own_first = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Controller first-spell witness",
            true,
            "You gain 0 life.",
        )
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let own_next_turn = scenario
        .add_spell_to_hand_from_oracle(P0, "Controller next-turn witness", true, "You gain 0 life.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let foreign_first = scenario
        .add_spell_to_hand_from_oracle(P1, "Opponent first-spell witness", true, "You gain 0 life.")
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let foreign_next_turn = scenario
        .add_spell_to_hand_from_oracle(P1, "Opponent next-turn witness", true, "You gain 0 life.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    for seat in [P0, P1] {
        for _ in 0..3 {
            scenario.add_card_to_library_top(seat, "Library padding witness");
        }
    }
    scenario.with_mana_pool(P0, pool(2, 0));
    scenario.with_mana_pool(P1, pool(3, 0));
    let mut runner = scenario.build();
    begin(&mut runner, own_first);
    lord_target(&mut runner, P0, P1);
    complete(&mut runner);
    assert_eq!(runner.state().players[1].life, 18);
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    begin(&mut runner, foreign_first);
    lord_target(&mut runner, P1, P0);
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 17);
    let turn = runner.state().turn_number;
    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player: P0, .. }
    ));
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("real empty combat declaration");
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().turn_number, turn + 1);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    // Pool funding is scenario setup; all four named spell entries still pay
    // their actual costs through the ordinary cast authority.
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(0),
        false,
        vec![],
    ));
    runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(0),
        false,
        vec![],
    ));
    begin(&mut runner, foreign_next_turn);
    lord_target(&mut runner, P1, P0);
    complete(&mut runner);
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    begin(&mut runner, own_next_turn);
    lord_target(&mut runner, P0, P1);
    complete(&mut runner);
    assert_eq!(runner.state().players[0].life, 16);
    assert_eq!(runner.state().players[1].life, 17);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
}

#[test]
fn puca_death_counters_bound_announced_return_before_the_selected_card() {
    // CR 122.2 + CR 603.10a + CR 601.2c: announcement reads the actual departure snapshot.
    use engine::types::counter::CounterType;
    let database = super::search_result_conditional_destination::load_finite_printed_records(
        EVENT_TARGET_COHORT_PRINTED,
    );
    let printed = database.get_face_by_name("Puca's Covenant").unwrap();
    assert!(engine::game::coverage::card_face_gaps(printed).is_empty());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_enchantment_from_oracle(P0, "Puca's Covenant", "Whenever a creature you control with a counter on it dies, you may return another target permanent card with mana value less than or equal to the number of counters on that creature from your graveyard to your hand. Do this only once each turn.").id();
    let dying = scenario
        .add_creature_from_oracle(
            P0,
            "Actual sacrificed creature",
            2,
            2,
            "Sacrifice this creature: You gain 1 life.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_counter(dying, CounterType::Generic("charge".into()), 3);
    let chosen = scenario
        .add_artifact_from_oracle(P0, "Zero-counter MV two", "")
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let high = scenario
        .add_artifact_from_oracle(P0, "Over-cap MV four", "")
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let decoy = scenario
        .add_creature(P0, "Unrelated seven-counter creature", 2, 2)
        .id();
    scenario.with_counter(decoy, CounterType::Generic("charge".into()), 7);
    let mut runner = scenario.build();
    engine::game::printed_cards::apply_card_face_to_object(
        runner.state_mut().objects.get_mut(&source).unwrap(),
        printed,
    );
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), source);
    for id in [chosen, high] {
        move_to_zone(runner.state_mut(), id, Zone::Graveyard, &mut vec![]);
    }
    runner
        .act(GameAction::ActivateAbility {
            source_id: dying,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Puca's real death-trigger target announcement: {:?}",
            runner.state().waiting_for
        );
    };
    let legal = &target_slots[selection.current_slot].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(chosen)),
        "three death counters admit MV two despite zero returned-card counters"
    );
    assert!(
        !legal.contains(&TargetRef::Object(high)),
        "unrelated live counters cannot expand the death-event cap"
    );
    assert!(
        !legal.contains(&TargetRef::Object(dying)),
        "another excludes the dying card, not the enchantment"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .unwrap();
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == source)
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    complete(&mut runner);
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&high].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&dying].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.life(P0), 21);
}

#[test]
fn combat_step_head_fires_before_declaration_only_on_its_own_turn() {
    // CR 500.1 + CR 603.2b: a beginning-of-combat trigger precedes attackers on that turn.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oracle = "At the beginning of your combat step, tap Aggressive Crag.\n{T}: Add {R} or {W}.";
    let own = scenario
        .add_land_from_oracle(P0, "Aggressive Crag", oracle)
        .id();
    let foreign = scenario
        .add_land_from_oracle(P1, "Aggressive Crag", oracle)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player: P0, .. }
    ));
    assert!(runner.state().objects[&own].tapped);
    assert!(!runner.state().objects[&foreign].tapped);
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

// Complete unchanged records from pinned MTGJSON 2026-10-04, version 5.3.0+20261004.
const EVENT_TARGET_COHORT_PRINTED: &[&str] = &[
    r####"{"colorIdentity":["G"],"colors":["G"],"convertedManaCost":3.0,"edhrecRank":8350,"foreignData":[{"language":"French","name":"Pacte du pucca","skuIds":{"nonfoil":"d3f614c0-1bb6-53b9-8010-e30c28f17f72"},"text":"\u00c0 chaque fois qu'une cr\u00e9ature que vous contr\u00f4lez avec un marqueur sur elle meurt, vous pouvez renvoyer dans votre main une autre carte de permanent cibl\u00e9e avec une valeur de mana inf\u00e9rieure ou \u00e9gale au nombre de marqueurs sur cette cr\u00e9ature depuis votre cimeti\u00e8re. Ne faites ceci qu'une seule fois par tour."},{"language":"German","name":"Pakt mit dem Phooka","skuIds":{"nonfoil":"11424111-3f70-5093-b7ad-6b78bf7e4cca"},"text":"Immer wenn eine Kreatur, die du kontrollierst und auf der eine Marke liegt, stirbt, kannst du eine andere Permanent-Karte deiner Wahl mit Manabetrag kleiner oder gleich der Anzahl an Marken auf jener Kreatur aus deinem Friedhof auf deine Hand zur\u00fcckbringen. Tue dies nur einmal pro Zug."},{"language":"Italian","name":"Accordo del Puca","skuIds":{"nonfoil":"f282c2cb-757b-5fce-a5f7-a762304952f6"},"text":"Ogniqualvolta una creatura con un segnalino che controlli muore, puoi riprendere in mano dal tuo cimitero un'altra carta permanente bersaglio con valore di mana pari o inferiore al numero di segnalini su quella creatura. Fallo solo una volta per turno."},{"language":"Japanese","name":"\u30d7\u30fc\u30ab\u306e\u8a93\u7d04","skuIds":{"nonfoil":"5c1e1fdd-096c-5531-9280-29245673e7f8"},"text":"\u3042\u306a\u305f\u304c\u30b3\u30f3\u30c8\u30ed\u30fc\u30eb\u3057\u3066\u3044\u3066\u30ab\u30a6\u30f3\u30bf\u30fc\u304c\u7f6e\u304b\u308c\u3066\u3044\u308b\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\uff11\u4f53\u304c\u6b7b\u4ea1\u3059\u308b\u305f\u3073\u3001\u3042\u306a\u305f\u306e\u5893\u5730\u306b\u3042\u308a\u3001\u30de\u30ca\u7dcf\u91cf\u304c\u305d\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u306e\u4e0a\u306b\u3042\u3063\u305f\u30ab\u30a6\u30f3\u30bf\u30fc\u306e\u500b\u6570\u4ee5\u4e0b\u3067\u3042\u308a\u305d\u308c\u3067\u306a\u3044\u30d1\u30fc\u30de\u30cd\u30f3\u30c8\u30fb\u30ab\u30fc\u30c9\uff11\u679a\u3092\u5bfe\u8c61\u3068\u3059\u308b\u3002\u305d\u308c\u3092\u3042\u306a\u305f\u306e\u624b\u672d\u306b\u623b\u3057\u3066\u3082\u3088\u3044\u3002\u3053\u308c\u306f\u6bce\u30bf\u30fc\u30f3\uff11\u56de\u3057\u304b\u884c\u3048\u306a\u3044\u3002"},{"language":"Spanish","name":"Pacto de puca","skuIds":{"nonfoil":"b737c275-b998-5ff4-b23c-a270c182e585"},"text":"Siempre que una criatura que controlas con un contador sobre ella muera, puedes regresar de tu cementerio a tu mano otra carta de permanente objetivo con valor de man\u00e1 menor o igual que la cantidad de contadores sobre esa criatura. Haz esto solo una vez por turno."}],"identifiers":{"scryfallOracleId":"9b832f87-0bd5-47bd-ab9c-718bd9a6c441"},"layout":"normal","legalities":{"commander":"Legal","duel":"Legal","legacy":"Legal","oathbreaker":"Legal","vintage":"Legal"},"manaCost":"{2}{G}","manaValue":3.0,"name":"Puca's Covenant","printings":["ECC"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/e44fabd2565d9bee","cardmarket":"https://mtgjson.com/links/b5e46b6a42086d96","tcgplayer":"https://mtgjson.com/links/2e47e81ad3748cbb"},"rulings":[{"date":"2025-11-17","text":"If a permanent card in a graveyard has {X} in its mana cost, X is 0 for the purpose of determining its mana value."}],"subtypes":[],"supertypes":[],"text":"Whenever a creature you control with a counter on it dies, you may return another target permanent card with mana value less than or equal to the number of counters on that creature from your graveyard to your hand. Do this only once each turn.","type":"Enchantment","types":["Enchantment"]}"####,
    r####"{"colorIdentity":["G","R"],"colors":["G","R"],"convertedManaCost":4.0,"edhrecRank":17781,"edhrecSaltiness":0.13,"foreignData":[{"language":"Chinese Simplified","name":"\u788e\u5b81\u602a\u78e8","skuIds":{"foil":"8fd21b56-50c3-5f15-9836-3d9299c8ca61","nonfoil":"b0083cb2-88ef-520e-9964-3382c35435c5"},"text":"\u5728\u4f60\u7684\u7ed3\u675f\u6b65\u9aa4\u5f00\u59cb\u65f6\uff0c\u717d\u60d1\u6bcf\u4e2a\u7531\u5bf9\u624b\u64cd\u63a7\u4e14\u5176\u4e0a\u7ed3\u9644\u6709\u7531\u4f60\u64cd\u63a7\u4e4b\u7075\u6c14\u7684\u751f\u7269\u3002\u6bcf\u4ee5\u6b64\u6cd5\u717d\u60d1\u4e00\u4e2a\u751f\u7269\uff0c\u4fbf\u5728\u788e\u5b81\u602a\u78e8\u4e0a\u653e\u7f6e\u4e00\u4e2a+1/+1\u6307\u793a\u7269\u3002\uff08\u76f4\u5230\u4f60\u7684\u4e0b\u4e00\u4e2a\u56de\u5408\uff0c\u5df2\u717d\u60d1\u7684\u751f\u7269\u6bcf\u6b21\u6218\u6597\u82e5\u80fd\u653b\u51fb\uff0c\u5219\u5fc5\u987b\u653b\u51fb\uff0c\u4e14\u82e5\u80fd\u653b\u51fb\u9664\u4f60\u4ee5\u5916\u7684\u724c\u624b\uff0c\u5219\u5fc5\u987b\u5982\u6b64\u4f5c\u3002\uff09","type":"\u4f20\u5947\u751f\u7269 \uff5e\u7cbe\u602a"},{"language":"Chinese Traditional","name":"\u788e\u5be7\u602a\u78e8","skuIds":{"foil":"f4db3dae-9000-5f75-b4a0-445e411e5f37","nonfoil":"6f51a2d6-0e28-5acc-986c-0107f33c2d94"},"text":"\u5728\u4f60\u7684\u7d50\u675f\u6b65\u9a5f\u958b\u59cb\u6642\uff0c\u717d\u60d1\u6bcf\u500b\u7531\u5c0d\u624b\u64cd\u63a7\u4e14\u5176\u4e0a\u7d50\u9644\u6709\u7531\u4f60\u64cd\u63a7\u4e4b\u9748\u6c23\u7684\u751f\u7269\u3002\u6bcf\u4ee5\u6b64\u6cd5\u717d\u60d1\u4e00\u500b\u751f\u7269\uff0c\u4fbf\u5728\u788e\u5be7\u602a\u78e8\u4e0a\u653e\u7f6e\u4e00\u500b+1/+1\u6307\u793a\u7269\u3002\uff08\u76f4\u5230\u4f60\u7684\u4e0b\u4e00\u500b\u56de\u5408\uff0c\u6240\u717d\u60d1\u7684\u751f\u7269\u6bcf\u6b21\u6230\u9b25\u82e5\u80fd\u653b\u64ca\uff0c\u5247\u5fc5\u9808\u653b\u64ca\uff0c\u4e14\u82e5\u80fd\u653b\u64ca\u9664\u4f60\u4ee5\u5916\u7684\u73a9\u5bb6\uff0c\u5247\u5fc5\u9808\u5982\u6b64\u4f5c\u3002\uff09","type":"\u50b3\u5947\u751f\u7269 \uff5e\u7cbe\u9748"},{"language":"French","name":"Kaima, le calme fractur\u00e9","skuIds":{"foil":"2167de77-2903-5a21-a647-9d127fae0ba1","nonfoil":"ac4a4448-9643-50a1-b463-9f5ec9fe5ce0"},"text":"Au d\u00e9but de votre \u00e9tape de fin, incitez chaque cr\u00e9ature que vos adversaires contr\u00f4lent qui est enchant\u00e9e par une aura que vous contr\u00f4lez. Mettez un marqueur +1/+1 sur Kaima, le calme fractur\u00e9 pour chaque cr\u00e9ature incit\u00e9e de cette mani\u00e8re. (Jusqu'\u00e0 votre prochain tour, les cr\u00e9atures incit\u00e9es attaquent \u00e0 chaque combat si possible et attaquent un joueur autre que vous si possible.)","type":"Cr\u00e9ature l\u00e9gendaire : esprit"},{"language":"German","name":"Kaima, die zerbrochene Ruhe","skuIds":{"foil":"411ef7a5-b249-5238-8c06-2cce98d95cbd","nonfoil":"97792c5b-fd32-54db-81bf-143d9f8feaf3"},"text":"Zu Beginn deines Endsegments stachelst du jede Kreatur an, die ein Gegner kontrolliert und die von einer Aura, die du kontrollierst, verzaubert wird. Lege f\u00fcr jede Kreatur, die auf diese Weise angestachelt wurde, eine +1/+1-Marke auf Kaima, die zerbrochene Ruhe. (Bis zu deinem n\u00e4chsten Zug greifen die angestachelten Kreaturen in jedem Kampf an, falls m\u00f6glich, und greifen einen anderen Spieler als dich an, falls m\u00f6glich.)","type":"Legend\u00e4re Kreatur \u2014 Geist"},{"language":"Italian","name":"Kaima, la Calma Fratturata","skuIds":{"foil":"1ab876a6-ef0a-5530-bb12-8eb73b838247","nonfoil":"5237ecd4-c251-55eb-aaff-d45d456a782d"},"text":"All'inizio della tua sottofase finale, sprona ogni creatura controllata dai tuoi avversari che \u00e8 incantata da un'Aura che controlli. Metti un segnalino +1/+1 su Kaima, la Calma Fratturata per ogni creatura spronata in questo modo. (Fino al tuo prossimo turno, le creature spronate attaccano in ogni combattimento un giocatore diverso da te, se possono farlo.)","type":"Creatura Leggendaria \u2014 Spirito"},{"language":"Japanese","name":"\u93ae\u307e\u3089\u306c\u3082\u306e\u3001\u602a\u78e8","skuIds":{"foil":"d776093b-f284-505a-adc6-99041539fffd","nonfoil":"b01ebbe6-039b-56b7-8bbd-68b60eab79ca"},"text":"\u3042\u306a\u305f\u306e\u7d42\u4e86\u30b9\u30c6\u30c3\u30d7\u306e\u958b\u59cb\u6642\u306b\u3001\u3042\u306a\u305f\u304c\u30b3\u30f3\u30c8\u30ed\u30fc\u30eb\u3057\u3066\u3044\u308b\u30aa\u30fc\u30e9\u304c\u3064\u3044\u3066\u3044\u3066\u5bfe\u6226\u76f8\u624b\u304c\u30b3\u30f3\u30c8\u30ed\u30fc\u30eb\u3057\u3066\u3044\u308b\u3059\u3079\u3066\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u3092\u4f7f\u55fe\u3059\u308b\u3002\u93ae\u307e\u3089\u306c\u3082\u306e\u3001\u602a\u78e8\u306e\u4e0a\u306b\u3001\u3053\u308c\u306b\u3088\u308a\u4f7f\u55fe\u3057\u305f\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\uff11\u4f53\u306b\u3064\u304d\uff11\u500b\u306e\uff0b\uff11/\uff0b\uff11\u30ab\u30a6\u30f3\u30bf\u30fc\u3092\u7f6e\u304f\u3002\uff08\u6b21\u306e\u3042\u306a\u305f\u306e\u30bf\u30fc\u30f3\u307e\u3067\u3001\u5404\u6226\u95d8\u3067\u3001\u4f7f\u55fe\u3055\u308c\u305f\u30af\u30ea\u30fc\u30c1\u30e3\u30fc\u306f\u53ef\u80fd\u306a\u3089\u653b\u6483\u3057\u3001\u53ef\u80fd\u306a\u3089\u3042\u306a\u305f\u4ee5\u5916\u306e\u30d7\u30ec\u30a4\u30e4\u30fc\u3092\u653b\u6483\u3059\u308b\u3002\uff09","type":"\u4f1d\u8aac\u306e\u30af\u30ea\u30fc\u30c1\u30e3\u30fc \u2014 \u30b9\u30d4\u30ea\u30c3\u30c8"},{"language":"Korean","name":"\ud30c\uc5f4\ub41c \ud3c9\uc628, \uce74\uc774\ub9c8","skuIds":{"foil":"ae1ff2b7-867a-52ac-9c16-cc3dbb3fe501","nonfoil":"b6f86ae2-6139-5be8-8e18-45f64425eaf7"},"text":"\ub2f9\uc2e0\uc758 \uc885\ub8cc\ub2e8 \uc2dc\uc791\uc5d0, \ub2f9\uc2e0\uc774 \uc870\uc885\ud558\ub294 \ub9c8\ubc95\uc9c4\uc774 \ubd80\uc5ec\ub418\uc5b4 \uc788\uc73c\uba70 \ub2f9\uc2e0\uc758 \uc0c1\ub300\ub4e4\uc774 \uc870\uc885\ud558\ub294 \uac01 \uc0dd\ubb3c\uc744 \uc790\uadf9\ud55c\ub2e4. \uc774\ub7f0 \uc2dd\uc73c\ub85c \uc790\uadf9\ub41c \uac01 \uc0dd\ubb3c\ub9c8\ub2e4 \ud30c\uc5f4\ub41c \ud3c9\uc628, \uce74\uc774\ub9c8\uc5d0 +1/+1 \uce74\uc6b4\ud130\ub97c \ud55c \uac1c\uc529 \uc62c\ub824\ub193\ub294\ub2e4. (\ub2f9\uc2e0\uc758 \ub2e4\uc74c \ud134\uae4c\uc9c0, \uc790\uadf9\ub41c \uc0dd\ubb3c\ub4e4\uc740 \uac00\ub2a5\ud558\uba74 \ub9e4 \uc804\ud22c\ub9c8\ub2e4 \uacf5\uaca9\ud558\uace0 \uac00\ub2a5\ud558\uba74 \ub2f9\uc2e0\uc774 \uc544\ub2cc \ud50c\ub808\uc774\uc5b4\ub97c \uacf5\uaca9\ud55c\ub2e4.)","type":"\uc804\uc124\uc801 \uc0dd\ubb3c \u2014 \uc2e0\ub839"},{"language":"Portuguese (Brazil)","name":"Kaima, a Calma Fraturada","skuIds":{"foil":"9f6113ae-fc6b-53f5-9ca3-05b9e4fab3d3","nonfoil":"bd67c4d8-bf77-5de2-826b-a7601638492f"},"text":"No in\u00edcio de sua etapa final, atice cada criatura que seus oponentes controlam que esteja encantada por uma Aura que voc\u00ea controle. Coloque um marcador +1/+1 em Kaima, a Calma Fraturada, para cada criatura ati\u00e7ada dessa forma. (At\u00e9 seu pr\u00f3ximo turno, as criaturas ati\u00e7adas atacam a cada combate se estiverem aptas e atacam um jogador que n\u00e3o seja voc\u00ea se estiverem aptas.)","type":"Criatura Lend\u00e1ria \u2014 Esp\u00edrito"},{"language":"Russian","name":"\u041a\u0430\u0438\u043c\u0430, \u041d\u0430\u0440\u0443\u0448\u0435\u043d\u043d\u044b\u0439 \u041f\u043e\u043a\u043e\u0439","skuIds":{"foil":"795c1801-1d3b-57d6-b891-de75b0482b1a","nonfoil":"23d3fb31-ae6f-5dd2-95df-93084e52e3a7"},"text":"\u0412 \u043d\u0430\u0447\u0430\u043b\u0435 \u0432\u0430\u0448\u0435\u0433\u043e \u0437\u0430\u043a\u043b\u044e\u0447\u0438\u0442\u0435\u043b\u044c\u043d\u043e\u0433\u043e \u0448\u0430\u0433\u0430 \u0440\u0430\u0437\u044a\u044f\u0440\u0438\u0442\u0435 \u043a\u0430\u0436\u0434\u043e\u0435 \u0441\u0443\u0449\u0435\u0441\u0442\u0432\u043e \u043f\u043e\u0434 \u043a\u043e\u043d\u0442\u0440\u043e\u043b\u0435\u043c \u0432\u0430\u0448\u0438\u0445 \u043e\u043f\u043f\u043e\u043d\u0435\u043d\u0442\u043e\u0432, \u043a\u043e\u0442\u043e\u0440\u043e\u0435 \u0437\u0430\u0447\u0430\u0440\u043e\u0432\u0430\u043d\u043e \u0410\u0443\u0440\u043e\u0439 \u043f\u043e\u0434 \u0432\u0430\u0448\u0438\u043c \u043a\u043e\u043d\u0442\u0440\u043e\u043b\u0435\u043c. \u041f\u043e\u043b\u043e\u0436\u0438\u0442\u0435 \u043e\u0434\u0438\u043d \u0436\u0435\u0442\u043e\u043d +1/+1 \u043d\u0430 \u041a\u0430\u0438\u043c\u0443, \u041d\u0430\u0440\u0443\u0448\u0435\u043d\u043d\u044b\u0439 \u041f\u043e\u043a\u043e\u0439 \u0437\u0430 \u043a\u0430\u0436\u0434\u043e\u0435 \u0441\u0443\u0449\u0435\u0441\u0442\u0432\u043e, \u0440\u0430\u0437\u044a\u044f\u0440\u0435\u043d\u043d\u043e\u0435 \u0442\u0430\u043a\u0438\u043c \u043e\u0431\u0440\u0430\u0437\u043e\u043c. (\u0414\u043e \u0432\u0430\u0448\u0435\u0433\u043e \u0441\u043b\u0435\u0434\u0443\u044e\u0449\u0435\u0433\u043e \u0445\u043e\u0434\u0430 \u0440\u0430\u0437\u044a\u044f\u0440\u0435\u043d\u043d\u044b\u0435 \u0441\u0443\u0449\u0435\u0441\u0442\u0432\u0430 \u0430\u0442\u0430\u043a\u0443\u044e\u0442 \u0432 \u043a\u0430\u0436\u0434\u043e\u043c \u0431\u043e\u044e, \u0435\u0441\u043b\u0438 \u043c\u043e\u0433\u0443\u0442, \u0438 \u0430\u0442\u0430\u043a\u0443\u044e\u0442 \u043d\u0435 \u0432\u0430\u0441, \u0430 \u0434\u0440\u0443\u0433\u043e\u0433\u043e \u0438\u0433\u0440\u043e\u043a\u0430, \u0435\u0441\u043b\u0438 \u043c\u043e\u0433\u0443\u0442.)","type":"\u041b\u0435\u0433\u0435\u043d\u0434\u0430\u0440\u043d\u043e\u0435 \u0421\u0443\u0449\u0435\u0441\u0442\u0432\u043e \u2014 \u0414\u0443\u0445"},{"language":"Spanish","name":"Kaima, la Calma Perdida","skuIds":{"foil":"2405b06f-d95d-53b4-90fb-913ec85b0a87","nonfoil":"288136a6-6623-54c5-94aa-e4da02e049a4"},"text":"Al comienzo de tu paso final, incita a cada criatura que controlan tus oponentes que est\u00e9 encantada por un Aura que controlas. Pon un contador +1/+1 sobre Kaima, la Calma Perdida por cada criatura incitada de esta manera. (Hasta tu pr\u00f3ximo turno, las criaturas incitadas atacan cada combate si pueden y atacan a un jugador que no seas t\u00fa si pueden.)","type":"Criatura legendaria \u2014 Esp\u00edritu"}],"identifiers":{"scryfallOracleId":"3d8f9b58-d011-4ed2-99ae-c626243fab26"},"keywords":["Goad"],"layout":"normal","leadershipSkills":{"brawl":false,"commander":true,"oathbreaker":false,"pauper_commander":false,"predh":false},"legalities":{"commander":"Legal","duel":"Legal","legacy":"Legal","oathbreaker":"Legal","vintage":"Legal"},"manaCost":"{2}{R}{G}","manaValue":4.0,"name":"Kaima, the Fractured Calm","power":"3","printings":["NEC"],"purchaseUrls":{"cardKingdom":"https://mtgjson.com/links/24f58b0de127defb","cardKingdomFoil":"https://mtgjson.com/links/9cfda05d9ce94c1b","cardmarket":"https://mtgjson.com/links/f6e87c861413929d","cardmarketFoil":"https://mtgjson.com/links/2be82774078a06d5","tcgplayer":"https://mtgjson.com/links/13993f574ef2266e"},"rulings":[{"date":"2022-02-18","text":"The set of creatures that are goaded by this ability are locked in when it resolves. A creature becoming enchanted later or an Aura being removed from a creature later won't change whether it is goaded."}],"subtypes":["Spirit"],"supertypes":["Legendary"],"text":"At the beginning of your end step, goad each creature your opponents control that's enchanted by an Aura you control. Put a +1/+1 counter on Kaima for each creature goaded this way. (Until your next turn, the goaded creatures attack each combat if able and attack a player other than you if able.)","toughness":"3","type":"Legendary Creature \u2014 Spirit","types":["Creature"]}"####,
];

#[test]
fn kaima_zero_eligible_creatures_cannot_count_a_previous_goad_or_a_foreign_aura() {
    use engine::game::effects::attach::attach_to;
    let database = super::search_result_conditional_destination::load_finite_printed_records(
        EVENT_TARGET_COHORT_PRINTED,
    );
    let face = database
        .get_face_by_name("Kaima, the Fractured Calm")
        .unwrap();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let kaima = scenario.add_creature(P0, &face.name, 3, 3).id();
    let previous = scenario.add_creature(P1, "Already goaded decoy", 2, 2).id();
    let foreign_host = scenario.add_creature(P1, "Opponent Aura host", 2, 2).id();
    let own_host = scenario.add_creature(P0, "Own Aura host", 2, 2).id();
    let foreign_aura = scenario
        .add_enchantment_from_oracle(P1, "Foreign Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let own_aura = scenario
        .add_enchantment_from_oracle(P0, "Own Aura", "Enchant creature")
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    engine::game::printed_cards::apply_card_face_to_object(
        runner.state_mut().objects.get_mut(&kaima).unwrap(),
        face,
    );
    engine::game::trigger_index::reindex_object_triggers(runner.state_mut(), kaima);
    runner
        .state_mut()
        .objects
        .get_mut(&previous)
        .unwrap()
        .goaded_by
        .insert(P0);
    attach_to(runner.state_mut(), foreign_aura, foreign_host);
    attach_to(runner.state_mut(), own_aura, own_host);
    for _ in 0..16 {
        if !runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().phase, Phase::End);
    assert!(
        !runner.state().stack.is_empty(),
        "the printed end-step trigger is reached even with an empty result"
    );
    complete(&mut runner);
    assert!(runner.state().objects[&previous].goaded_by.contains(&P0));
    assert!(!runner.state().objects[&foreign_host]
        .goaded_by
        .contains(&P0));
    assert!(!runner.state().objects[&own_host].goaded_by.contains(&P0));
    assert_eq!(
        runner.state().objects[&kaima]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "count only this resolution's exact empty population"
    );
}

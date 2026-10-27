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

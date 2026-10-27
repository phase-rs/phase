//! Defining envelopes through paid public announcement and resolution.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use nom::{combinator::all_consuming, Parser};

pub(super) fn cost(text: &str) -> ManaCost {
    all_consuming(engine::parser::oracle_nom::primitives::parse_mana_cost)
        .parse(text)
        .unwrap()
        .1
}

pub(super) fn fund(runner: &mut GameRunner, generic: usize, colors: &[ManaType]) {
    for color in std::iter::repeat_n(ManaType::Colorless, generic).chain(colors.iter().copied()) {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

pub(super) fn settled(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().active_ability_continuation().is_none());
}

#[test]
fn verse_sources_capture_zero_or_two_before_the_actual_sacrifice() {
    use engine::types::counter::CounterType;
    // CR 601.2c: the target bound is announced before the source is sacrificed.
    for (name, oracle, color, generic, kind) in [
        ("Rumbling Crescendo", "At the beginning of your upkeep, you may put a verse counter on this enchantment.\n{R}, Sacrifice this enchantment: Destroy up to X target lands, where X is the number of verse counters on this enchantment.", ManaType::Red, 0, 0),
        ("Serra's Liturgy", "At the beginning of your upkeep, you may put a verse counter on this enchantment.\n{W}, Sacrifice this enchantment: Destroy up to X target artifacts and/or enchantments, where X is the number of verse counters on this enchantment.", ManaType::White, 0, 1),
        ("Vile Requiem", "At the beginning of your upkeep, you may put a verse counter on this enchantment.\n{1}{B}, Sacrifice this enchantment: Destroy up to X target nonblack creatures, where X is the number of verse counters on this enchantment. They can't be regenerated.", ManaType::Black, 1, 2),
    ] {
        for count in [0, 2] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario.add_enchantment_from_oracle(P0, name, oracle).id();
            let decoy = scenario.add_enchantment_from_oracle(P0, name, oracle).id();
            scenario.with_counter(source, CounterType::Generic("verse".into()), count);
            scenario.with_counter(decoy, CounterType::Generic("verse".into()), 7);
            let first = match kind {
                0 => scenario.add_basic_land(P1, ManaColor::Green),
                1 => scenario.add_artifact_from_oracle(P1, "First artifact", "").id(),
                _ => scenario.add_creature(P1, "First nonblack", 2, 2).id(),
            };
            let second = match kind {
                0 => scenario.add_basic_land(P1, ManaColor::Blue),
                1 => scenario.add_enchantment_from_oracle(P1, "Second enchantment", "").id(),
                _ => scenario.add_creature(P1, "Second nonblack", 3, 3).id(),
            };
            let third = match kind {
                0 => scenario.add_basic_land(P1, ManaColor::White),
                1 => scenario.add_artifact_from_oracle(P1, "Overbound third artifact", "").id(),
                _ => scenario.add_creature(P1, "Overbound third nonblack", 4, 4).id(),
            };
            let mut runner = scenario.build();
            fund(&mut runner, generic, &[color]);
            runner.act(GameAction::ActivateAbility { source_id: source, ability_index: 0, payment_mode: engine::types::game_state::CastPaymentMode::Auto }).unwrap();
            if count == 0 {
                assert!(!matches!(runner.state().waiting_for, WaitingFor::TargetSelection { .. }), "zero-counter source does not borrow decoy's seven-counter bound");
            } else {
                let WaitingFor::TargetSelection { player, target_slots, .. } = &runner.state().waiting_for else { panic!("actual pre-sacrifice target declaration"); };
                assert_eq!(*player, P0);
                assert_eq!(target_slots.len(), 2, "actual source's two counters, not seven-counter same-name decoy");
                assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield, "announcement precedes sacrifice payment");
                runner.act(GameAction::ChooseTarget { target: Some(engine::types::ability::TargetRef::Object(second)) }).unwrap();
                runner.act(GameAction::ChooseTarget { target: None }).unwrap();
            }
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard, "real source paid as cost");
            assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&third].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&second].zone, if count == 0 { Zone::Battlefield } else { Zone::Graveyard });
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            settled(&runner);
        }
    }
}

#[test]
fn orcish_settlers_pays_both_x_components_and_sacrifices_its_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Orcish Settlers",
            1,
            1,
            "{X}{X}{R}, {T}, Sacrifice this creature: Destroy X target lands.",
        )
        .id();
    let first = scenario.add_basic_land(P1, ManaColor::Blue);
    let second = scenario.add_basic_land(P1, ManaColor::Green);
    let third = scenario.add_basic_land(P1, ManaColor::White);
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Red]);
    runner
        .activate(source, 0)
        .x(2)
        .target_objects(&[second, first])
        .resolve();
    for id in [source, first, second] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(runner.state().objects[&third].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
}

#[test]
fn additional_sacrifice_x_is_the_actual_destroy_target_cardinality() {
    for (name, text, cost, generic, colors) in [
        ("Eliminate the Competition", "As an additional cost to cast this spell, sacrifice X creatures.\nDestroy X target creatures.", "{4}{B}", 4, vec![ManaType::Black]),
        ("Immoral Bargain", "As an additional cost to cast this spell, sacrifice X creatures.\nDestroy X target nonland permanents.", "{1}{B}{G}", 1, vec![ManaType::Black, ManaType::Green]),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let pay1 = scenario.add_creature(P0, "First payment", 1, 1).id();
        let pay2 = scenario.add_creature(P0, "Second payment", 1, 1).id();
        let victim1 = scenario.add_creature(P1, "First victim", 2, 2).id();
        let victim2 = scenario.add_creature(P1, "Second victim", 3, 3).id();
        let decoy = scenario.add_creature(P1, "Unselected victim", 4, 4).id();
        let spell = scenario.add_spell_to_hand_from_oracle(P0, name, false, text)
            .with_mana_cost(self::cost(cost)).id();
        let mut runner = scenario.build();
        fund(&mut runner, generic, &colors);
        runner.cast(spell).x(2).sacrifice_with(&[pay2, pay1]).target_objects(&[victim2, victim1]).resolve();
        for id in [pay1, pay2, victim1, victim2, spell] { assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard); }
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        settled(&runner);
    }
}

#[test]
fn diaochan_keeps_each_real_chooser_in_a_three_player_activation() {
    use engine::game::engine::apply;
    use engine::types::ability::TargetRef;
    use engine::types::game_state::CastPaymentMode;
    use engine::types::player::PlayerId;
    for late_first in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 24);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature_from_oracle(P0, "Diaochan, Artful Beauty", 1, 1,
        "{T}: Destroy target creature of your choice, then destroy target creature of an opponent's choice. Activate only during your turn, before attackers are declared.").as_legendary().id();
        let first = scenario.add_creature(P1, "First chosen", 2, 2).id();
        let second = scenario
            .add_creature(PlayerId(2), "Second chosen", 3, 3)
            .id();
        let decoy = scenario.add_creature(P1, "Unselected", 4, 4).id();
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection { player: P0, .. } = &runner.state().waiting_for else {
            panic!(
                "first actual activation target prompt: {:?}",
                runner.state().waiting_for
            );
        };
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(first)),
            })
            .unwrap();
        let actor = runner
            .state()
            .waiting_for
            .acting_player()
            .expect("second chooser");
        assert_ne!(actor, P0);
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(apply(
            runner.state_mut(),
            P0,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second))
            }
        )
        .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        apply(
            runner.state_mut(),
            actor,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second)),
            },
        )
        .unwrap();
        if late_first {
            engine::game::zones::move_to_zone(runner.state_mut(), first, Zone::Hand, &mut vec![]);
        }
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&first].zone,
            if late_first {
                Zone::Hand
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
        assert!(runner.state().objects[&source].tapped);
        settled(&runner);
    }
}

#[test]
fn rampaging_yao_guai_uses_total_mana_value_instead_of_target_count() {
    // CR 107.3m: the ETB budget and entry counters use the spell's paid X.
    for empty in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario
            .add_artifact_from_oracle(P1, "MV one", "")
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let b = scenario
            .add_enchantment_from_oracle(P1, "MV three", "")
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let bad = scenario
            .add_artifact_from_oracle(P1, "MV five", "")
            .with_mana_cost(ManaCost::generic(5))
            .id();
        let zero = scenario
            .add_artifact_from_oracle(P1, "MV zero", "")
            .with_mana_cost(ManaCost::zero())
            .id();
        let spell = scenario.add_creature_to_hand_from_oracle(P0, "Rampaging Yao Guai", 2, 2,
            "Vigilance, trample\nThis creature enters with X +1/+1 counters on it.\nWhen this creature enters, destroy any number of target artifacts and/or enchantments with total mana value X or less.")
            .with_mana_cost(cost("{X}{G}{G}{G}")).id();
        let mut runner = scenario.build();
        fund(&mut runner, 4, &[ManaType::Green; 3]);
        runner.cast(spell).x(4).commit();
        runner.resolve_top();
        let WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            player,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real Yao Guai ETB targets");
        };
        assert_eq!(*player, P0);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&engine::types::ability::TargetRef::Object(a)));
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&engine::types::ability::TargetRef::Object(zero)));
        assert!(!target_slots[selection.current_slot]
            .legal_targets
            .contains(&engine::types::ability::TargetRef::Object(bad)));
        if !empty {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(zero)),
                })
                .unwrap();
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(b)),
                })
                .unwrap();
            assert!(runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(bad))
                })
                .is_err());
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(a)),
                })
                .unwrap();
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget { target: None })
                .unwrap();
        }
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&a].zone,
            if empty {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(
            runner.state().objects[&b].zone,
            if empty {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().objects[&bad].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&zero].zone,
            if empty {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(
            runner.state().objects[&spell]
                .counters
                .get(&engine::types::counter::CounterType::Plus1Plus1),
            Some(&4)
        );
        for keyword in [
            engine::types::keywords::Keyword::Vigilance,
            engine::types::keywords::Keyword::Trample,
        ] {
            assert!(runner.state().objects[&spell].keywords.contains(&keyword));
        }
        settled(&runner);
    }
}

#[test]
fn lorwyn_live_land_bound_keeps_non_elf_opponent_target_restrictions() {
    use engine::types::ability::TargetRef;
    use engine::types::card_type::CoreType;
    for remove_land in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let lands: Vec<_> = (0..3)
            .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
            .collect();
        let target = scenario.add_creature(P1, "Non-Elf power3", 3, 3).id();
        let elf = scenario
            .add_creature(P1, "Elf power1", 1, 1)
            .with_subtypes(vec!["Elf"])
            .id();
        let above = scenario.add_creature(P1, "Above bound", 4, 4).id();
        let own = scenario.add_creature(P0, "Own power1", 1, 1).id();
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Invasion of Lorwyn", false,
        "(As a Siege enters, choose an opponent to protect it. You and others can attack it. When it's defeated, exile it, then cast it transformed.)\nWhen this Siege enters, destroy target non-Elf creature an opponent controls with power X or less, where X is the number of lands you control.").with_mana_cost(cost("{4}{B}{G}")).id();
        let mut runner = scenario.build();
        {
            let object = runner.state_mut().objects.get_mut(&spell).unwrap();
            object.card_types.core_types = vec![CoreType::Battle];
            object.card_types.subtypes = vec!["Siege".into()];
            object.base_card_types = object.card_types.clone();
            object.defense = Some(5);
            object.base_defense = Some(5);
        }
        fund(&mut runner, 4, &[ManaType::Black, ManaType::Green]);
        runner.cast(spell).commit();
        runner.resolve_top();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::BattleProtectorChoice { .. }
        ) {
            runner
                .act(GameAction::ChooseBattleProtector { protector: P1 })
                .unwrap();
        }
        let WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("real Lorwyn destroy announcement");
        };
        assert_eq!(*player, P0);
        let legal = &target_slots[selection.current_slot].legal_targets;
        assert!(legal.contains(&TargetRef::Object(target)));
        for id in [elf, above, own] {
            assert!(!legal.contains(&TargetRef::Object(id)));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
        if remove_land {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                lands[0],
                Zone::Hand,
                &mut vec![],
            );
        }
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&target].zone,
            if remove_land {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            "live bound falls to2 before legality recheck beside the successful bound3 control"
        );
        settled(&runner);
    }
}

#[test]
fn wall_other_player_paid_activation_preserves_independent_exact_self_destruction() {
    use engine::types::game_state::CastPaymentMode;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_from_oracle(P0, "Wall of Vipers", 2, 4,
        "Defender (This creature can't attack.)\n{3}: Destroy this creature and target creature it's blocking. Any player may activate this ability.").id();
    let decoy = scenario.add_creature(P0, "Wall of Vipers", 2, 4).id();
    let mut runner = scenario.build();
    for _ in 0..3 {
        runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    runner.act(GameAction::PassPriority).unwrap();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
    settled(&runner);
}

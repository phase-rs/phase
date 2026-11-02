//! Bare choose instructions resolve without targeting; explicit targets do not.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetChoiceTiming, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FINAL_SHOWDOWN: &str = "Spree (Choose one or more additional costs.)\n+ {1} — All creatures lose all abilities until end of turn.\n+ {1} — Choose a creature you control. It gains indestructible until end of turn.\n+ {3}{W}{W} — Destroy all creatures.";

#[test]
fn final_showdown_chooses_shroud_creature_only_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shroud = scenario
        .add_creature(P0, "Shroud creature", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let hexproof = scenario
        .add_creature(P0, "Hexproof creature", 2, 2)
        .with_keyword(Keyword::Hexproof)
        .id();
    let opposing = scenario.add_creature(P1, "Opposing creature", 2, 2).id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Showdown", true, FINAL_SHOWDOWN)
        .with_mana_cost(parse_mtgjson_mana_cost("{W}"))
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .unwrap();
    // CR 115.10 + CR 115.10a: the mode has no cast-time target.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, spell);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);

    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "must reach the creature choice"
        );
        runner.act(GameAction::PassPriority).unwrap();
    }
    // CR 608.2d: only now is a creature chosen, without target restrictions.
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        up_to,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected resolution creature choice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, spell);
    assert_eq!(*count, 1);
    assert!(!*up_to);
    assert!(cards.contains(&shroud));
    assert!(cards.contains(&hexproof));
    assert!(!cards.contains(&opposing));
    runner
        .act(GameAction::SelectCards {
            cards: vec![shroud],
        })
        .unwrap();
    assert!(runner.state().objects[&shroud].has_keyword(&Keyword::Indestructible));
    assert!(!runner.state().objects[&hexproof].has_keyword(&Keyword::Indestructible));
    assert!(!runner.state().objects[&opposing].has_keyword(&Keyword::Indestructible));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn explicit_target_spell_prompts_at_cast_and_excludes_shroud() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shroud = scenario
        .add_creature(P0, "Shroud creature", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let hexproof = scenario
        .add_creature(P0, "Hexproof creature", 2, 2)
        .with_keyword(Keyword::Hexproof)
        .id();
    let plain = scenario.add_creature(P0, "Plain creature", 2, 2).id();
    scenario.add_basic_land(P0, ManaColor::Green);
    // Giant Growth is the printed explicit-target control; Final Showdown has no such mode.
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // CR 601.2c: a printed target is announced as part of casting.
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected cast-time target selection: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(hexproof)));
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(plain)));
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(shroud)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(hexproof)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&hexproof].power, Some(5));
    assert_eq!(runner.state().objects[&shroud].power, Some(2));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn mid_chain_bare_choose_does_not_hide_later_cast_time_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own creature", 2, 2).id();
    let other_own = scenario.add_creature(P0, "Other own creature", 2, 2).id();
    let opposing = scenario.add_creature(P1, "Opposing creature", 3, 3).id();
    let other_opposing = scenario
        .add_creature(P1, "Other opposing creature", 3, 3)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Mid-chain choice",
            true,
            "Draw a card. Choose a creature you control. It deals damage equal to its power to target creature you don't control.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // Mid-chain bare choices retain their legacy Stack timing. Answer that
    // slot, if offered first, to inspect the later printed target itself.
    if let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for {
        if selection
            .current_legal_targets
            .contains(&TargetRef::Object(own))
            && !selection
                .current_legal_targets
                .contains(&TargetRef::Object(opposing))
        {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(own)),
                })
                .unwrap();
        }
    }
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the later printed target while casting: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(opposing)),
        "the printed opponent-creature target must actually be offered"
    );
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(other_opposing)));
    assert!(
        !selection
            .current_legal_targets
            .contains(&TargetRef::Object(own)),
        "the bare choose must not replace the printed target slot"
    );
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(other_own)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(opposing)),
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, spell);
    let root = entry
        .ability()
        .expect("the cast spell must carry its ability");
    let damage = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
        .find(|node| matches!(node.effect, Effect::DealDamage { .. }))
        .expect("the cast spell must retain its printed damage instruction");
    assert!(
        damage.targets.contains(&TargetRef::Object(opposing)),
        "the damage target must already be announced on the stack: {damage:#?}"
    );
    assert!(!damage.targets.contains(&TargetRef::Object(other_opposing)));
}

#[test]
fn sakashima_avatar_announces_copy_source_then_prompts_recipient_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let avatar = scenario
        .add_creature_from_oracle(
            P0,
            "Sakashima the Impostor Avatar",
            1,
            1,
            "{2}: Choose a creature you control. It becomes a copy of target creature except for its name.",
        )
        .id();
    let shroud = scenario
        .add_creature(P0, "Shroud recipient", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let copy_source = scenario.add_creature(P0, "Copy source", 5, 7).id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: avatar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected copy-source target during activation: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(copy_source)));
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(shroud)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(copy_source)),
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, avatar);
    let head = entry
        .ability()
        .expect("the activated ability must be on the stack");
    assert!(
        head.targets.is_empty(),
        "the resolution-time recipient must have no activation-time target slot"
    );
    let copy = head
        .sub_ability
        .as_deref()
        .expect("the copy-source instruction must remain on the stack");
    assert_eq!(copy.targets, vec![TargetRef::Object(copy_source)]);

    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "must reach the copy-recipient choice"
        );
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        up_to,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected untargeted copy-recipient choice during resolution: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, avatar);
    assert_eq!(*count, 1);
    assert!(!*up_to);
    assert!(cards.contains(&shroud));
    assert!(cards.contains(&copy_source));
    runner
        .act(GameAction::SelectCards {
            cards: vec![shroud],
        })
        .unwrap();
    // The chosen recipient does not yet become the copy: BecomeCopy reads an
    // Untargeted(ParentTarget) recipient from the child's own target list (the
    // copy source); the same happens on main with a Stack-timed recipient.
    // Tracked separately; this test covers choice timing only.
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn modal_bare_choose_mode_does_not_hide_later_mode_target() {
    const ORACLE: &str = "Spree (Choose one or more additional costs.)\n+ {1} — Draw a card.\n+ {1} — Choose a creature you control. It gains indestructible until end of turn.\n+ {1} — Deal 2 damage to target creature you don't control.";

    // [1, 2] keeps the bare choose at the combined root. Selecting mode 0 as
    // well makes it a sub-ability, exercising the mid-chain deferral predicate.
    for indices in [&[1, 2][..], &[0, 1, 2][..]] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Modal draw card"]);
        let own = scenario.add_creature(P0, "Own creature", 2, 2).id();
        let other_own = scenario.add_creature(P0, "Other own creature", 2, 2).id();
        let opposing = scenario.add_creature(P1, "Opposing creature", 3, 3).id();
        let other_opposing = scenario
            .add_creature(P1, "Other opposing creature", 3, 3)
            .id();
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::White);
        }
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Modal choice timing probe", true, ORACLE)
            .with_mana_cost(parse_mtgjson_mana_cost("{W}"))
            .id();
        let mut runner = scenario.build();
        let choice_mode = &runner.state().objects[&spell].abilities[1];
        assert!(
            matches!(choice_mode.effect.as_ref(), Effect::TargetOnly { .. }),
            "mode 1 must reach the bare-choice producer: {choice_mode:#?}"
        );
        assert_eq!(
            choice_mode.target_choice_timing,
            TargetChoiceTiming::Resolution
        );
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ModeChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::SelectModes {
                indices: indices.to_vec(),
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player, selection, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "{indices:?}: the later mode must announce its target while casting: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert!(
            selection
                .current_legal_targets
                .contains(&TargetRef::Object(opposing)),
            "{indices:?}: the later mode's opponent-creature target must be offered"
        );
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(other_opposing)));
        assert!(!selection
            .current_legal_targets
            .contains(&TargetRef::Object(own)));
        assert!(!selection
            .current_legal_targets
            .contains(&TargetRef::Object(other_own)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(opposing)),
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().stack.len(), 1);
        let entry = &runner.state().stack[0];
        assert_eq!(entry.source_id, spell);
        let root = entry
            .ability()
            .expect("the modal spell must carry its selected instructions");
        let damage = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
            .find(|node| matches!(node.effect, Effect::DealDamage { .. }))
            .expect("the selected damage mode must be on the stack");
        assert_eq!(
            damage.targets,
            vec![TargetRef::Object(opposing)],
            "{indices:?}: the damage target must already be announced"
        );

        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            assert!(
                !runner.state().stack.is_empty(),
                "{indices:?}: must reach the bare-choice mode"
            );
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            source_id,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "{indices:?}: the bare-choice mode must still prompt at resolution: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, spell);
        assert_eq!(*count, 1);
        assert!(!*up_to);
        assert!(cards.contains(&own));
        assert!(cards.contains(&other_own));
        assert!(!cards.contains(&opposing));
        assert!(!cards.contains(&other_opposing));
        assert_eq!(
            runner.state().objects[&opposing].damage_marked,
            0,
            "{indices:?}: the later damage mode must wait for the choice"
        );
        runner
            .act(GameAction::SelectCards {
                cards: vec![other_own],
            })
            .unwrap();
        assert!(runner.state().objects[&other_own].has_keyword(&Keyword::Indestructible));
        assert!(!runner.state().objects[&own].has_keyword(&Keyword::Indestructible));
        assert_eq!(runner.state().objects[&opposing].damage_marked, 2);
        assert_eq!(runner.state().objects[&other_opposing].damage_marked, 0);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn head_bare_choose_does_not_hide_later_printed_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plain = scenario.add_creature(P0, "Plain creature", 2, 8).id();
    let shroud = scenario
        .add_creature(P0, "Shroud creature", 5, 8)
        .with_keyword(Keyword::Shroud)
        .id();
    let target = scenario.add_creature(P1, "Opposing target", 1, 12).id();
    let other_opposing = scenario.add_creature(P1, "Other target", 3, 12).id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Head choice",
            true,
            "Choose a creature you control. It deals damage equal to its power to target creature you don't control.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the printed opposing-creature target while casting: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(target)));
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(other_opposing)));
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(plain)));
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(shroud)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, spell);
    let head = entry
        .ability()
        .expect("the head-choice spell must be on the stack");
    assert!(matches!(head.effect, Effect::TargetOnly { .. }));
    assert_eq!(head.target_choice_timing, TargetChoiceTiming::Resolution);
    assert!(head.targets.is_empty());
    let damage = head
        .sub_ability
        .as_deref()
        .expect("the printed damage instruction must remain on the stack");
    assert!(matches!(damage.effect, Effect::DealDamage { .. }));
    assert_eq!(damage.target_choice_timing, TargetChoiceTiming::Stack);
    assert_eq!(damage.targets, vec![TargetRef::Object(target)]);

    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "must reach the creature choice"
        );
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        up_to,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the untargeted creature choice during resolution: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, spell);
    assert_eq!(*count, 1);
    assert!(!*up_to);
    assert!(cards.contains(&plain));
    assert!(cards.contains(&shroud));
    assert!(!cards.contains(&target));
    assert!(!cards.contains(&other_opposing));
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
    runner
        .act(GameAction::SelectCards {
            cards: vec![shroud],
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&target].damage_marked,
        5,
        "the damage must use the picked shroud creature's power, not the plain creature's 2 power"
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&plain].damage_marked, 0);
    assert_eq!(runner.state().objects[&shroud].damage_marked, 0);
    assert_eq!(runner.state().objects[&other_opposing].damage_marked, 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

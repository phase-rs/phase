//! Bare choose instructions resolve without targeting; explicit targets do not.

use std::collections::HashSet;

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetChoiceTiming, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

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

/// Synthetic Oracle: no printed card pairs a bare choose in a later chunk with a
/// distinct later printed target through this path. The nearest printed card is
/// Crush Underfoot ("Choose a Giant creature you control. It deals damage equal
/// to its power to target creature."), whose choice opens the spell.
/// Pins two facts (CR 115.10a + CR 608.2d): a bare choose in a LATER chunk
/// resolves at resolution time, with no cast-time prompt of its own, and the later
/// printed target (CR 601.2c) is still the only cast-time prompt. At resolution
/// the controller picks among their own creatures only, and the damage uses the
/// picked creature's power.
#[test]
fn later_chunk_bare_choose_resolves_at_resolution_and_later_printed_target_is_announced() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The spell's Draw must not empty the library and lose the game.
    scenario.with_library_top(P0, &["Library card"]);
    // Different powers, so the damage tells which creature was bound.
    let own = scenario.add_creature(P0, "Own creature", 2, 2).id();
    let other_own = scenario.add_creature(P0, "Other own creature", 4, 4).id();
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
    // CR 601.2c: the first and only cast-time prompt is the printed target slot;
    // the bare choose in the later chunk has no slot of its own.
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the printed target while casting: {:?}",
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
    // No further cast-time prompt: the spell is on the stack.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, spell);
    let root = entry
        .ability()
        .expect("the cast spell must carry its ability");
    let choice = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
        .find(|node| matches!(node.effect, Effect::TargetOnly { .. }))
        .expect("the cast spell must retain its bare-choice instruction");
    assert_eq!(choice.target_choice_timing, TargetChoiceTiming::Resolution);
    assert!(choice.targets.is_empty());
    let damage = std::iter::successors(Some(root), |node| node.sub_ability.as_deref())
        .find(|node| matches!(node.effect, Effect::DealDamage { .. }))
        .expect("the cast spell must retain its printed damage instruction");
    assert_eq!(damage.target_choice_timing, TargetChoiceTiming::Stack);
    assert!(
        damage.targets.contains(&TargetRef::Object(opposing)),
        "the damage target must already be announced on the stack: {damage:#?}"
    );
    assert!(!damage.targets.contains(&TargetRef::Object(other_opposing)));

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
    // CR 608.2d: only now does the controller choose, among their own creatures.
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
    assert!(cards.contains(&own));
    assert!(cards.contains(&other_own));
    assert!(!cards.contains(&opposing));
    assert!(!cards.contains(&other_opposing));
    assert_eq!(runner.state().objects[&opposing].damage_marked, 0);
    runner
        .act(GameAction::SelectCards { cards: vec![own] })
        .unwrap();
    assert_eq!(
        runner.state().objects[&opposing].damage_marked,
        2,
        "the damage must use the picked 2-power creature, not the 4-power one"
    );
    assert_eq!(runner.state().objects[&other_opposing].damage_marked, 0);
    assert_eq!(runner.state().objects[&own].damage_marked, 0);
    assert_eq!(runner.state().objects[&other_own].damage_marked, 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
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
    // This test covers choice timing only, not the BecomeCopy result. The
    // recipient does not yet become the copy; whether the same holds on main with
    // a Stack-timed recipient has not been run.
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Synthetic Oracle: no printed card pairs a bare-choice mode with a later targeted
/// mode through this path; nearest analogue is Final Showdown's spree modes.
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

/// Synthetic Oracle: no printed card covers this distinct later-target path;
/// nearest analogue is Crush Underfoot, whose target is mis-lowered on main.
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

// ---------------------------------------------------------------------------
// Printed cards whose bare, untargeted "choose a <object> you control" sits in
// a LATER chunk of the chain (CR 115.10a + CR 608.2d): the choice is made while
// the effect resolves, never while the spell or ability is put on the stack.
// ---------------------------------------------------------------------------

/// Pass priority until the next non-priority prompt. Every pass is made with a
/// non-empty stack, so a resolution that finishes without prompting fails here
/// instead of being papered over by a further pass.
fn pass_priority_to_prompt(runner: &mut GameRunner, what: &str) {
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "{what}: the stack emptied before the expected prompt appeared: {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{what}: no prompt appeared within 8 priority passes"
    );
}

/// The resolution finished: nothing left on the stack and no pending prompt.
/// An empty stack alone can coexist with a suspended resolution.
fn assert_resolution_settled(runner: &GameRunner, what: &str) {
    assert!(
        runner.state().stack.is_empty(),
        "{what}: the stack must be empty"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{what}: expected priority with no pending prompt, got {:?}",
        runner.state().waiting_for
    );
}

fn id_set(ids: &[ObjectId]) -> HashSet<ObjectId> {
    ids.iter().copied().collect()
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

const THE_ELDERSPELL: &str = "Destroy any number of target planeswalkers. Choose a planeswalker you control. Put two loyalty counters on it for each planeswalker destroyed this way.";
const WALKER_ABILITY: &str = "+1: You gain 1 life.";

/// The Elderspell: the printed "any number of target planeswalkers" is announced
/// while casting (CR 601.2c); the later bare "Choose a planeswalker you control"
/// is made while the spell resolves (CR 115.10a + CR 608.2d) and offers only the
/// controller's own planeswalkers. The counters follow the number destroyed this
/// way (CR 701.8a), and loyalty is the number of loyalty counters (CR 306.5c).
#[test]
fn elderspell_counts_destroyed_planeswalkers_and_offers_only_own_planeswalkers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Different starting loyalties, so the counters tell which walker was bound.
    let own_low = scenario
        .add_planeswalker_from_oracle(P0, "Own Low Walker", "Jace", 5, WALKER_ABILITY)
        .id();
    let own_high = scenario
        .add_planeswalker_from_oracle(P0, "Own High Walker", "Chandra", 6, WALKER_ABILITY)
        .id();
    let victim = scenario
        .add_planeswalker_from_oracle(P1, "Opposing Victim Walker", "Liliana", 4, WALKER_ABILITY)
        .id();
    let spared = scenario
        .add_planeswalker_from_oracle(P1, "Opposing Spared Walker", "Nissa", 7, WALKER_ABILITY)
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Elderspell", false, THE_ELDERSPELL)
        .with_mana_cost(parse_mtgjson_mana_cost("{B}{B}"))
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

    // CR 601.2c: only the printed target is announced while casting.
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the printed planeswalker targets while casting: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(victim)),
        "the opposing victim must be a legal printed target"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .unwrap();
    // "Any number of": every remaining slot is optional, and is skipped so that
    // exactly one planeswalker is destroyed.
    for _ in 0..8 {
        let WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            break;
        };
        assert!(
            target_slots[selection.current_slot].optional,
            "every slot after the first must be optional for \"any number of\" targets"
        );
        runner
            .act(GameAction::ChooseTarget { target: None })
            .unwrap();
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "all target slots must be answered before the spell is on the stack: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, spell);
    assert_eq!(
        entry
            .ability()
            .expect("the cast spell must carry its ability")
            .targets,
        vec![TargetRef::Object(victim)],
        "exactly one planeswalker must be announced as the victim"
    );

    pass_priority_to_prompt(&mut runner, "The Elderspell");
    // CR 701.8a: the announced planeswalker was destroyed before the choice.
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spared].zone, Zone::Battlefield);
    // CR 608.2d: only now does the controller choose, among their own walkers.
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
            "expected the untargeted planeswalker choice during resolution: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, spell);
    assert_eq!(*count, 1);
    assert!(!*up_to);
    assert_eq!(
        id_set(cards),
        id_set(&[own_low, own_high]),
        "only the controller's own surviving planeswalkers may be offered"
    );
    assert!(!cards.contains(&victim));
    assert!(!cards.contains(&spared));
    assert_eq!(loyalty(&runner, own_low), 5);
    assert_eq!(loyalty(&runner, own_high), 6);
    assert_eq!(loyalty(&runner, spared), 7);

    runner
        .act(GameAction::SelectCards {
            cards: vec![own_high],
        })
        .unwrap();
    assert_eq!(
        loyalty(&runner, own_high),
        8,
        "two loyalty counters for the one planeswalker destroyed this way"
    );
    assert_eq!(loyalty(&runner, own_low), 5);
    assert_eq!(loyalty(&runner, spared), 7);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_resolution_settled(&runner, "The Elderspell");
}

const BRUDICLAD_ORACLE: &str = "Creature tokens you control have haste.\nAt the beginning of combat on your turn, create a 2/1 blue Phyrexian Myr artifact creature token. Then you may choose a token you control. If you do, each other token you control becomes a copy of that token.";
const BRUDICLAD_NAME: &str = "Brudiclad, Telchor Engineer";
const MYR_NAME: &str = "Phyrexian Myr";

struct BrudicladBoard {
    runner: GameRunner,
    brudiclad: ObjectId,
    /// A pre-existing token P0 controls, distinguishable from the new Myr.
    river_spirit: ObjectId,
    /// A token the opponent controls: never offered, never copied over.
    opposing_token: ObjectId,
}

/// Brudiclad's beginning-of-combat trigger has created its Myr token and is
/// parked on the optional "you may choose a token you control" prompt.
fn brudiclad_at_optional_choice() -> (BrudicladBoard, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brudiclad = scenario
        .add_creature_from_oracle(P0, BRUDICLAD_NAME, 4, 4, BRUDICLAD_ORACLE)
        .as_artifact_creature()
        .as_legendary()
        .with_subtypes(vec!["Phyrexian", "Artificer"])
        .id();
    let river_spirit = scenario.add_creature(P0, "River Spirit", 1, 1).id();
    let opposing_token = scenario.add_creature(P1, "Opposing Token", 3, 3).id();
    let mut runner = scenario.build();
    // `GameScenario` has no token helper: flag the staged creatures as tokens.
    for token in [river_spirit, opposing_token] {
        runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    }

    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0, .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, brudiclad);
    let myr_count = |runner: &GameRunner| {
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[*id].name == MYR_NAME)
            .count()
    };
    assert_eq!(myr_count(&runner), 0, "the Myr is created by the trigger");

    pass_priority_to_prompt(&mut runner, "Brudiclad trigger");
    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the optional token choice during resolution: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, brudiclad);
    // The token exists before the optional instruction is answered.
    assert_eq!(myr_count(&runner), 1, "exactly one Myr must be created");
    let myr = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[*id].name == MYR_NAME)
        .expect("the Myr token must be on the battlefield");
    let myr_object = &runner.state().objects[&myr];
    assert!(myr_object.is_token);
    assert_eq!(myr_object.controller, P0);
    assert_eq!((myr_object.power, myr_object.toughness), (Some(2), Some(1)));

    (
        BrudicladBoard {
            runner,
            brudiclad,
            river_spirit,
            opposing_token,
        },
        myr,
    )
}

/// Layers are evaluated before the characteristics of a copy are read.
fn name_and_pt(runner: &mut GameRunner, id: ObjectId) -> (String, Option<i32>, Option<i32>) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let object = &runner.state().objects[&id];
    (object.name.clone(), object.power, object.toughness)
}

/// Brudiclad: the bare "you may choose a token you control" follows the token
/// creation in the same chain (CR 115.10a + CR 608.2d). Accepting offers only
/// the controller's own tokens, and "each other token you control becomes a copy
/// of that token" (CR 707.2) turns the other own token into a copy.
#[test]
fn brudiclad_optional_token_choice_offers_new_token_and_copies_on_accept() {
    let (
        BrudicladBoard {
            mut runner,
            brudiclad,
            river_spirit,
            opposing_token,
        },
        myr,
    ) = brudiclad_at_optional_choice();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "accepting must prompt for the token to copy: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, brudiclad);
    assert_eq!(*count, 1);
    assert_eq!(
        id_set(cards),
        id_set(&[myr, river_spirit]),
        "exactly the controller's tokens, including the one just created"
    );
    assert!(!cards.contains(&brudiclad));
    assert!(!cards.contains(&opposing_token));
    // Before the choice, the other token is still itself.
    assert_eq!(
        name_and_pt(&mut runner, river_spirit),
        ("River Spirit".to_string(), Some(1), Some(1))
    );

    runner
        .act(GameAction::SelectCards { cards: vec![myr] })
        .unwrap();
    assert_eq!(
        name_and_pt(&mut runner, river_spirit),
        (MYR_NAME.to_string(), Some(2), Some(1)),
        "the other own token must become a copy of the chosen Myr"
    );
    assert_eq!(
        name_and_pt(&mut runner, myr),
        (MYR_NAME.to_string(), Some(2), Some(1))
    );
    assert_eq!(
        name_and_pt(&mut runner, opposing_token),
        ("Opposing Token".to_string(), Some(3), Some(3)),
        "the opponent's token is not \"a token you control\""
    );
    assert_eq!(
        name_and_pt(&mut runner, brudiclad),
        (BRUDICLAD_NAME.to_string(), Some(4), Some(4)),
        "Brudiclad is not a token and keeps its own characteristics"
    );
    assert_resolution_settled(&runner, "Brudiclad accepted");
}

/// Brudiclad, declined: the optional choice is made as the trigger resolves
/// (CR 603.5) and no token is chosen, so nothing becomes a copy even though two
/// eligible tokens are on the board.
#[test]
fn brudiclad_optional_token_choice_decline_skips_copy() {
    let (
        BrudicladBoard {
            mut runner,
            brudiclad,
            river_spirit,
            opposing_token,
        },
        myr,
    ) = brudiclad_at_optional_choice();
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    // Priority (not ChooseFromZoneChoice) is the declined-path boundary.
    assert_resolution_settled(&runner, "Brudiclad declined");
    assert_eq!(
        name_and_pt(&mut runner, river_spirit),
        ("River Spirit".to_string(), Some(1), Some(1)),
        "declining must leave the other own token unchanged"
    );
    assert_eq!(
        name_and_pt(&mut runner, myr),
        (MYR_NAME.to_string(), Some(2), Some(1))
    );
    assert_eq!(
        name_and_pt(&mut runner, opposing_token),
        ("Opposing Token".to_string(), Some(3), Some(3))
    );
    assert_eq!(
        name_and_pt(&mut runner, brudiclad),
        (BRUDICLAD_NAME.to_string(), Some(4), Some(4))
    );
}

const CANOPTEK_WRAITH_ORACLE: &str = "Wraith Form — This creature can't be blocked.\nTransdimensional Scout — When this creature deals combat damage to a player, you may pay {3} and sacrifice it. If you do, choose a land you control. Then search your library for up to two basic land cards which have the same name as the chosen land, put them onto the battlefield tapped, then shuffle.";

struct WraithBoard {
    runner: GameRunner,
    wraith: ObjectId,
    own_lands: Vec<ObjectId>,
    opposing_land: ObjectId,
    library_plains: [ObjectId; 2],
    library_island: ObjectId,
}

/// The Wraith has connected with P1 and its trigger is parked on the optional
/// "you may pay {3} and sacrifice it" prompt. P0 controls one basic land per
/// entry of `own_land_colors`; P1 controls a land; P0's library holds exactly
/// two basic Plains and one basic Island.
fn wraith_at_payment_prompt(own_land_colors: &[ManaColor]) -> WraithBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wraith = scenario
        .add_creature_from_oracle(P0, "Canoptek Wraith", 2, 1, CANOPTEK_WRAITH_ORACLE)
        .as_artifact_creature()
        .with_subtypes(vec!["Wraith"])
        .id();
    let mut own_lands = Vec::new();
    for color in own_land_colors {
        own_lands.push(scenario.add_basic_land(P0, *color));
    }
    let opposing_land = scenario.add_basic_land(P1, ManaColor::Green);
    let plains_a = scenario.add_land_to_library_top(P0, "Plains").id();
    let plains_b = scenario.add_land_to_library_top(P0, "Plains").id();
    let island = scenario.add_land_to_library_top(P0, "Island").id();
    let mut runner = scenario.build();
    // `add_land_to_library_top` only sets the Land type: a basic-land search
    // also needs the Basic supertype and the name-matching land subtype.
    for (id, subtype) in [
        (plains_a, "Plains"),
        (plains_b, "Plains"),
        (island, "Island"),
    ] {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.card_types.supertypes.push(Supertype::Basic);
        object.card_types.subtypes.push(subtype.to_string());
        object.base_card_types = object.card_types.clone();
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        3,
        "the staged lands must be P0's entire library"
    );

    run_combat(&mut runner, vec![wraith], vec![]);
    // Reach guard: the unblocked Wraith connected, so its trigger exists.
    assert_eq!(runner.life(P1), 18, "the Wraith must deal 2 combat damage");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, wraith);
    pass_priority_to_prompt(&mut runner, "Canoptek Wraith trigger");
    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the optional payment prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, wraith);

    WraithBoard {
        runner,
        wraith,
        own_lands,
        opposing_land,
        library_plains: [plains_a, plains_b],
        library_island: island,
    }
}

/// Canoptek Wraith: "you may pay {3} and sacrifice it. If you do, choose a land
/// you control" (CR 118.12 + CR 701.21a). Declining reaches neither the land
/// choice nor the search; paying offers exactly the controller's lands
/// (CR 115.10a and CR 608.2d), then searches for basic lands with the chosen
/// land's name (CR 701.23a) and puts them onto the battlefield tapped.
#[test]
fn canoptek_wraith_declined_payment_gives_no_land_choice_and_paid_payment_offers_own_lands() {
    // Declined. Two own lands with different names: a wrongly reached choice
    // would prompt rather than bind silently to a lone candidate.
    let WraithBoard {
        mut runner,
        wraith,
        own_lands,
        opposing_land,
        library_plains,
        library_island,
    } = wraith_at_payment_prompt(&[ManaColor::White, ManaColor::Blue]);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_resolution_settled(&runner, "Canoptek Wraith declined");
    assert_eq!(runner.state().objects[&wraith].zone, Zone::Battlefield);
    for land in own_lands.iter().chain([&opposing_land]) {
        assert_eq!(runner.state().objects[land].zone, Zone::Battlefield);
        assert!(
            !runner.state().objects[land].tapped,
            "declining must not pay {{3}}"
        );
    }
    for card in library_plains.iter().chain([&library_island]) {
        assert_eq!(runner.state().objects[card].zone, Zone::Library);
    }

    // Paid. Five own lands of distinct names, enough to pay {3}.
    let WraithBoard {
        mut runner,
        wraith,
        own_lands,
        opposing_land,
        library_plains,
        library_island,
    } = wraith_at_payment_prompt(&[
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    // CR 701.21a: the Wraith was sacrificed as part of the payment.
    assert_eq!(runner.state().objects[&wraith].zone, Zone::Graveyard);
    let tapped_for_payment = own_lands
        .iter()
        .filter(|land| runner.state().objects[*land].tapped)
        .count();
    assert_eq!(tapped_for_payment, 3, "{{3}} must have been paid");
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "paying must prompt for the land to name: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, wraith);
    assert_eq!(*count, 1);
    assert_eq!(
        id_set(cards),
        id_set(&own_lands),
        "exactly the controller's lands, tapped or not"
    );
    assert!(!cards.contains(&opposing_land));
    assert!(!cards.contains(&wraith));

    let chosen_land = own_lands[0];
    assert_eq!(runner.state().objects[&chosen_land].name, "Plains");
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen_land],
        })
        .unwrap();
    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        up_to,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the chosen land must lead to a library search: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(
        id_set(cards),
        id_set(&library_plains),
        "only basic lands with the chosen land's name may be found"
    );
    assert!(!cards.contains(&library_island));
    assert_eq!(*count, 2);
    assert!(*up_to);

    runner
        .act(GameAction::SelectCards {
            cards: library_plains.to_vec(),
        })
        .unwrap();
    for plains in library_plains {
        assert_eq!(runner.state().objects[&plains].zone, Zone::Battlefield);
        assert!(
            runner.state().objects[&plains].tapped,
            "the found lands enter tapped"
        );
        assert_eq!(runner.state().objects[&plains].controller, P0);
    }
    assert_eq!(runner.state().objects[&library_island].zone, Zone::Library);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![library_island],
        "only the unfound Island remains in the library"
    );
    assert_eq!(runner.state().objects[&wraith].zone, Zone::Graveyard);
    assert_resolution_settled(&runner, "Canoptek Wraith paid");
}

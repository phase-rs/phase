//! Bare choose instructions resolve without targeting; explicit targets do not.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
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

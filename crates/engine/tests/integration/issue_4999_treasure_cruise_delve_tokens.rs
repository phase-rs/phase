//! Regression for Treasure Cruise freezing when stale token/copy residents were
//! accepted as Delve payments after leaving the battlefield.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ConvokeMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TREASURE_CRUISE_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\nDraw three cards.";

fn blue_mana() -> ManaUnit {
    ManaUnit::new(
        ManaType::Blue,
        engine::types::identifiers::ObjectId(0),
        false,
        vec![],
    )
}

#[test]
fn delve_eligibility_and_actions_exclude_stale_noncard_graveyard_residents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let delve_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Delve Payment", false, TREASURE_CRUISE_ORACLE)
        .from_oracle_text_with_keywords(&["Delve"], TREASURE_CRUISE_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let real = scenario.add_spell_to_graveyard(P0, "Real Card", true).id();
    let token = scenario
        .add_spell_to_graveyard(P0, "Stale Treasure", true)
        .id();
    let copy = scenario.add_spell_to_graveyard(P0, "Stale Copy", true).id();
    let opponent = scenario
        .add_spell_to_graveyard(P1, "Opponent Card", true)
        .id();
    let wrong_zone = scenario.add_spell_to_hand(P0, "Hand Card", true).id();
    let mut runner = scenario.build();

    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    runner.state_mut().objects.get_mut(&copy).unwrap().is_copy = true;

    assert!(runner.state().objects[&real].is_delve_eligible(P0));
    assert!(!runner.state().objects[&token].is_delve_eligible(P0));
    assert!(!runner.state().objects[&copy].is_delve_eligible(P0));
    assert!(!runner.state().objects[&opponent].is_delve_eligible(P0));
    assert!(!runner.state().objects[&wrong_zone].is_delve_eligible(P0));

    let card_id = runner.state().objects[&delve_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: delve_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin a real Delve cast before simulating candidate actions");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment {
            convoke_mode: Some(ConvokeMode::Delve),
            ..
        }
    ));

    let (actions, _, _) = legal_actions_full(runner.state());
    let delve_actions = actions
        .iter()
        .filter_map(|action| match action {
            GameAction::TapForConvoke {
                object_id,
                mana_type: ManaType::Colorless,
            } => Some(*object_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(delve_actions, vec![real]);

    for invalid in [token, copy, opponent] {
        assert!(runner
            .act(GameAction::TapForConvoke {
                object_id: invalid,
                mana_type: ManaType::Colorless,
            })
            .is_err());
        assert_eq!(runner.state().objects[&invalid].zone, Zone::Graveyard);
    }
    assert!(runner
        .act(GameAction::TapForConvoke {
            object_id: real,
            mana_type: ManaType::Blue,
        })
        .is_err());
    assert_eq!(runner.state().objects[&real].zone, Zone::Graveyard);
    assert!(runner.state().players[P0.0 as usize]
        .mana_pool
        .mana
        .is_empty());

    runner
        .act(GameAction::TapForConvoke {
            object_id: real,
            mana_type: ManaType::Colorless,
        })
        .expect("a real card in the caster's graveyard can pay Delve");
    assert_eq!(runner.state().objects[&real].zone, Zone::Exile);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.mana.len(),
        1
    );
    assert!(runner
        .act(GameAction::TapForConvoke {
            object_id: real,
            mana_type: ManaType::Colorless,
        })
        .is_err());
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.mana.len(),
        1
    );
}

#[test]
fn treasure_cruise_delve_driver_exiles_only_cards_and_resolves_without_markers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cruise = scenario
        .add_spell_to_hand_from_oracle(P0, "Treasure Cruise", false, TREASURE_CRUISE_ORACLE)
        .from_oracle_text_with_keywords(&["Delve"], TREASURE_CRUISE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 7,
        })
        .id();
    let delved = (0..7)
        .map(|i| {
            scenario
                .add_spell_to_graveyard(P0, &format!("Delve Card {i}"), true)
                .id()
        })
        .collect::<Vec<_>>();
    for i in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Draw {i}"), true);
    }
    scenario.with_mana_pool(P0, vec![blue_mana()]);

    let mut runner = scenario.build();
    let outcome = runner.cast(cruise).delve_with(&delved).resolve();

    outcome.assert_hand_drawn(P0, 3);
    for card in delved {
        assert_eq!(outcome.zone_of(card), Zone::Exile);
    }
    assert!(outcome.state().players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .all(|unit| !unit.is_convoke_payment()));
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

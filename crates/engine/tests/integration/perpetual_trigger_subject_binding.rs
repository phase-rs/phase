//! PR #8494 regression: a bare `it` in a perpetual-grant body must keep the
//! trigger condition's object referent (CR 608.2k).
//!
//! Effluence Devourer's trigger is grammatically headed by `you`, so the parser
//! context's subject is a player filter even though the trigger's valid card is
//! the sacrificed creature. `resolve_it_pronoun` binds that antecedent to
//! `TriggeringSource`, whose runtime resolver understands
//! `GameEvent::PermanentSacrificed`; `ParentTarget` does not.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::PerpetualModification;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

#[test]
fn volatile_rift_modifies_the_library_to_hand_event_object_and_counts_only_own_colors() {
    use engine::types::ability::QuantityExpr;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    const RIFT: &str = "Vivid — Whenever a creature card is put into your hand from your library, it perpetually gets +X/+0, where X is the number of colors among permanents you control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let rift = scenario
        .add_enchantment_from_oracle(P0, "Volatile Rift", RIFT)
        .with_color(vec![ManaColor::Red, ManaColor::Green])
        .id();
    let own_decoy = scenario
        .add_creature(P0, "Own green decoy", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let opponent = scenario
        .add_creature(
            engine::game::scenario::P1,
            "Opponent white black decoy",
            2,
            2,
        )
        .with_color(vec![ManaColor::White, ManaColor::Black])
        .id();
    let existing_hand = scenario
        .add_creature_to_hand(P0, "Earlier hand creature", 2, 2)
        .id();
    let drawn = scenario
        .add_spell_to_library_top(P0, "Library creature", false)
        .as_creature()
        .id();
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Reach Through Mists", true, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(draw).resolve();
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert!(runner.state().objects[&drawn]
        .perpetual_mods
        .iter()
        .any(|modification| matches!(
            modification,
            PerpetualModification::ModifyPowerToughness {
                power: QuantityExpr::Fixed { value: 2 },
                toughness: QuantityExpr::Fixed { value: 0 },
                ..
            }
        )));
    for decoy in [rift, own_decoy, opponent, existing_hand] {
        assert!(runner.state().objects[&decoy].perpetual_mods.is_empty());
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

const EFFLUENCE_DEVOURER: &str = "Whenever you sacrifice Effluence Devourer or another creature, it perpetually gains \"{2}, Exile this card from your graveyard: Create an X/X green Ooze creature token, where X is this card's power. Activate only as a sorcery.\"";

fn has_granted_ability(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .perpetual_mods
        .iter()
        .any(|modification| matches!(modification, PerpetualModification::GrantAbility { .. }))
}

#[test]
fn effluence_devourer_grants_the_sacrificed_creature_not_the_trigger_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let devourer = scenario
        .add_creature_from_oracle(P0, "Effluence Devourer", 3, 4, EFFLUENCE_DEVOURER)
        .id();
    let victim = scenario.add_creature(P0, "Sacrifice Witness", 2, 2).id();
    let sacrifice = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Sacrifice", true, "Sacrifice a creature.")
        .id();

    let mut runner = scenario.build();
    runner.cast(sacrifice).effect_zone(&[victim]).resolve();

    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert!(
        has_granted_ability(&runner, victim),
        "the sacrificed creature is the trigger antecedent and receives the perpetual grant"
    );
    assert!(
        !has_granted_ability(&runner, devourer),
        "the trigger source must not receive a grant meant for the sacrificed creature"
    );
}

#[test]
fn paid_hand_choice_perpetual_edits_keep_the_newly_selected_card() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario::P1;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::QuantityExpr;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::{ManaType, ManaUnit};
    let db = export_route_repair_db();
    for (name, payment, delta) in [
        ("Sap Vitality", vec![ManaType::Black; 2], (3, 0)),
        (
            "Veteran Charger",
            vec![ManaType::Green, ManaType::Colorless, ManaType::Colorless],
            (2, 2),
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            payment
                .into_iter()
                .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
                .collect(),
        );
        let victim = scenario
            .add_creature(P1, "Prior damage recipient", 1, 8)
            .id();
        let earlier = scenario
            .add_creature_to_hand(P0, "Earlier eligible creature", 2, 2)
            .id();
        let selected = scenario
            .add_creature_to_hand(P0, "Selected hand creature", 3, 3)
            .id();
        let opposing = scenario
            .add_creature_to_hand(P1, "Opponent hand decoy", 4, 4)
            .id();
        let source = scenario.add_real_card(P0, name, Zone::Hand, &db).id();
        let mut runner = scenario.build();
        if name == "Sap Vitality" {
            runner.cast(source).target_object(victim).resolve();
        } else {
            runner.cast(source).resolve();
        }
        let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for else {
            panic!("the full printed hand choice must be reached");
        };
        assert!(cards.contains(&earlier) && cards.contains(&selected));
        assert!(!cards.contains(&opposing));
        runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert!(runner.state().objects[&selected].perpetual_mods.iter().any(|modification| matches!(modification,
            PerpetualModification::ModifyPowerToughness { power: QuantityExpr::Fixed { value: p }, toughness: QuantityExpr::Fixed { value: t }, .. } if (*p, *t) == delta)));
        for id in [earlier, opposing, victim, source] {
            assert!(runner.state().objects[&id].perpetual_mods.is_empty());
        }
        if name == "Sap Vitality" {
            assert_eq!(runner.state().objects[&victim].damage_marked, 3);
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn unsavory_kitchen_empty_hand_cannot_replace_the_selected_card_with_the_damage_event() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario::P1;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    let db = export_route_repair_db();
    for eligible in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );
        let kitchen = scenario
            .add_real_card(P0, "Unsavory Kitchen", Zone::Battlefield, &db)
            .id();
        let victim = scenario
            .add_creature(P1, "Damaged creature decoy", 2, 8)
            .id();
        let selected = eligible.then(|| {
            scenario
                .add_creature_to_hand(P0, "Eligible hand creature", 2, 2)
                .id()
        });
        let noncreature = scenario
            .add_spell_to_hand(P0, "Ineligible hand card", false)
            .id();
        let shock = scenario
            .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Red],
            })
            .id();
        let mut runner = scenario.build();
        runner.cast(shock).target_object(victim).resolve();
        assert_eq!(
            runner.state().objects[&victim].damage_marked,
            2,
            "trigger event reach guard"
        );
        if let Some(selected) = selected {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseFromZoneChoice { .. }
            ) {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![selected],
                    })
                    .unwrap();
                runner.advance_until_stack_empty();
            }
            assert!(
                !runner.state().objects[&selected].perpetual_mods.is_empty(),
                "positive chosen-card producer guard"
            );
        }
        for id in [kitchen, victim, noncreature] {
            assert!(runner.state().objects[&id].perpetual_mods.is_empty());
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    }
}

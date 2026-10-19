//! Runtime regression for #3270 — Extinction Event's odd/even choice.
//!
//! "Choose odd or even. Exile each creature with mana value of the chosen
//! quality."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EXTINCTION_EVENT: &str =
    "Choose odd or even. Exile each creature with mana value of the chosen quality. (Zero is even.)";

#[test]
fn extinction_event_exiles_only_the_chosen_mana_value_parity() {
    for choice in ["Odd", "Even"] {
        let mut scenario = GameScenario::new_n_player(2, 3270);
        scenario.at_phase(Phase::PreCombatMain);

        let odd_creature = scenario
            .add_creature(P0, "Odd Creature", 3, 3)
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let even_creature = scenario
            .add_creature(P1, "Even Creature", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let zero_creature = scenario
            .add_creature(P1, "Zero Creature", 1, 1)
            .with_mana_cost(ManaCost::zero())
            .id();

        let extinction_event = scenario
            .add_spell_to_hand_from_oracle(P0, "Extinction Event", false, EXTINCTION_EVENT)
            .with_mana_cost(ManaCost::zero())
            .id();

        let mut runner = scenario.build();

        runner.cast(extinction_event).resolve();
        let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
            panic!(
                "Extinction Event must pause on odd/even choice, got {}",
                runner.waiting_for_kind()
            );
        };
        assert!(options.iter().any(|choice| choice == "Odd"));
        assert!(options.iter().any(|choice| choice == "Even"));

        runner
            .act(GameAction::ChooseOption {
                choice: choice.to_string(),
            })
            .expect("ChooseOption(Odd) must resolve");
        runner.advance_until_stack_empty();

        let zone_of = |id| runner.state().objects[&id].zone;

        // CR 202.3 + CR 608.2c: the chosen odd/even quality filters mana value.
        assert_eq!(
            zone_of(odd_creature),
            if choice == "Odd" {
                Zone::Exile
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(
            zone_of(even_creature),
            if choice == "Even" {
                Zone::Exile
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(
            zone_of(zero_creature),
            if choice == "Even" {
                Zone::Exile
            } else {
                Zone::Battlefield
            }
        );
    }
}

#[test]
fn u14_local_choice_beats_durable_opposite_source() {
    use engine::types::ability::{ChosenAttribute, Parity};
    for choice in ["Odd", "Even"] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s.add_spell_to_hand(P0, "Parity Standard", false).as_enchantment()
            .from_oracle_text("As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.").id();
        let cards: Vec<_> = [P0, P1]
            .into_iter()
            .flat_map(|p| [0, 2, 3].into_iter().map(move |mv| (p, mv)))
            .map(|(p, mv)| {
                (
                    s.add_creature(p, "Exile candidate", 1, 2)
                        .with_mana_cost(ManaCost::generic(mv))
                        .id(),
                    mv,
                )
            })
            .collect();
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Extinction Event", false, EXTINCTION_EVENT)
            .id();
        let mut r = s.build();
        r.cast(source).resolve();
        assert_ne!(r.state().objects[&source].zone, Zone::Battlefield);
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::NamedChoice { .. }
        ));
        let opposite = if choice == "Odd" { "Even" } else { "Odd" };
        r.act(GameAction::ChooseOption {
            choice: opposite.into(),
        })
        .unwrap();
        r.advance_until_stack_empty();
        let expected = vec![ChosenAttribute::OddOrEven(if choice == "Odd" {
            Parity::Even
        } else {
            Parity::Odd
        })];
        assert_eq!(r.state().objects[&source].chosen_attributes, expected);
        r.cast(spell).resolve();
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::NamedChoice { .. }
        ));
        r.act(GameAction::ChooseOption {
            choice: choice.into(),
        })
        .unwrap();
        r.advance_until_stack_empty();
        for (id, mv) in cards {
            let exiled = (choice == "Even") == (mv % 2 == 0);
            assert_eq!(
                r.state().objects[&id].zone,
                if exiled {
                    Zone::Exile
                } else {
                    Zone::Battlefield
                }
            );
        }
        assert_eq!(r.state().objects[&source].chosen_attributes, expected);
        assert_eq!(r.state().objects[&source].zone, Zone::Battlefield);
    }
}

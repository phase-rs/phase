//! Whole population and controlled-attachment relations through ordinary actions.

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BALTHOR: &str = "Minion creatures get +1/+1.\n{B}{B}{B}, Exile Balthor: Each player returns all black and all red creature cards from their graveyard to the battlefield.";
const HAUNTED: &str = "Flying\nWhen this creature dies, exile it and each other player creates a 3/3 black Angel creature token with flying.";
const ARCHON: &str = "Flying\nOther creatures you control that are enchanted by Auras you control have base power and toughness 4/4 and have flying.";

#[test]
fn haunted_actual_death_exiles_the_successor_and_each_other_controller_gets_one_angel() {
    for (count, eliminated) in [(2, None), (3, None), (4, None), (4, Some(PlayerId(3)))] {
        let mut scenario = GameScenario::new_n_player(count, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let haunted = scenario
            .add_creature_from_oracle(P0, "Haunted Angel", 3, 3, HAUNTED)
            .controlled_by(P1)
            .id();
        let bolt = scenario.add_bolt_to_hand(P0);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        if let Some(player) = eliminated {
            engine::game::elimination::eliminate_player(
                runner.state_mut(),
                player,
                &mut Vec::new(),
            );
        }
        assert!(runner.state().objects[&haunted]
            .keywords
            .contains(&Keyword::Flying));
        let outcome = runner.cast(bolt).target_object(haunted).resolve();
        outcome.assert_zone(&[haunted], Zone::Exile);
        assert_eq!(outcome.state().objects[&haunted].owner, P0);
        let tokens: Vec<_> = outcome
            .state()
            .battlefield
            .iter()
            .filter_map(|id| {
                let object = &outcome.state().objects[id];
                (object.is_token && object.subtypes.iter().any(|subtype| subtype == "Angel"))
                    .then_some(object)
            })
            .collect();
        assert_eq!(
            tokens.len(),
            usize::from(count - 1) - usize::from(eliminated.is_some())
        );
        for seat in 0..count {
            let player = PlayerId(seat);
            let own: Vec<_> = tokens
                .iter()
                .filter(|object| object.controller == player)
                .collect();
            assert_eq!(
                own.len(),
                usize::from(player != P1 && Some(player) != eliminated)
            );
            for token in own {
                assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
                assert_eq!(token.colors, vec![ManaColor::Black]);
                assert!(token.keywords.contains(&Keyword::Flying));
                assert_eq!(token.owner, player);
            }
        }
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
    }
}

#[test]
fn haunted_unavailable_dying_source_does_not_gate_tokens_or_exile_a_later_birth() {
    for reenter in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let haunted = scenario
            .add_creature_from_oracle(P0, "Haunted Angel", 3, 3, HAUNTED)
            .controlled_by(P1)
            .id();
        let bolt = scenario.add_bolt_to_hand(P0);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
        );
        let mut runner = scenario.build();
        {
            let _committed = runner.cast(bolt).target_object(haunted).commit();
        }
        for _ in 0..3 {
            runner
                .act(engine::types::actions::GameAction::PassPriority)
                .unwrap();
        }
        assert_eq!(runner.state().objects[&haunted].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().stack.len(),
            1,
            "the actual death queued its independent printed trigger"
        );
        let dying_successor = runner.state().objects[&haunted].incarnation;
        engine::game::zones::move_to_zone(runner.state_mut(), haunted, Zone::Hand, &mut Vec::new());
        if reenter {
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                haunted,
                Zone::Battlefield,
                &mut Vec::new(),
            );
        }
        assert!(runner.state().objects[&haunted].incarnation > dying_successor);
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&haunted].zone,
            if reenter {
                Zone::Battlefield
            } else {
                Zone::Hand
            }
        );
        for player in [P0, P1, PlayerId(2)] {
            let angels: Vec<_> = runner
                .state()
                .battlefield
                .iter()
                .filter_map(|id| {
                    let object = &runner.state().objects[id];
                    (object.is_token
                        && object.controller == player
                        && object.subtypes.iter().any(|subtype| subtype == "Angel"))
                    .then_some(object)
                })
                .collect();
            assert_eq!(angels.len(), usize::from(player != P1));
            for angel in angels {
                assert_eq!((angel.power, angel.toughness), (Some(3), Some(3)));
                assert_eq!(angel.colors, vec![ManaColor::Black]);
                assert!(angel.keywords.contains(&Keyword::Flying));
            }
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn archon_paid_aura_delivery_and_live_both_controller_predicates_preserve_layers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archon = scenario
        .add_creature_from_oracle(P0, "Archon of the Wild Rose", 4, 4, ARCHON)
        .id();
    let own = scenario
        .add_creature(P0, "Enchanted own creature", 1, 2)
        .with_plus_counters(1)
        .id();
    let bare = scenario.add_creature(P0, "Own bare creature", 2, 3).id();
    let foreign = scenario.add_creature(P1, "Foreign creature", 2, 3).id();
    // Constructed Aura exercises normal cast and attachment without adding a
    // second power/toughness modifier to the printed Archon predicate.
    let aura = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic lawful creature Aura",
            false,
            "Enchant creature",
        )
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(aura).target_object(own).resolve();
    outcome.assert_power_toughness(own, 5, 5);
    outcome.assert_power_toughness(bare, 2, 3);
    outcome.assert_power_toughness(foreign, 2, 3);
    assert!(outcome.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    assert!(outcome.state().objects[&archon]
        .keywords
        .contains(&Keyword::Flying));
    assert_eq!(outcome.mana_pool_total(P0), 0);
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .controller = P1;
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .base_controller = Some(P1);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&own].power,
            runner.state().objects[&own].toughness
        ),
        (Some(2), Some(3))
    );
    assert!(!runner.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .controller = P0;
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .base_controller = Some(P0);
    attach_to(runner.state_mut(), aura, foreign);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&foreign].power,
            runner.state().objects[&foreign].toughness
        ),
        (Some(2), Some(3))
    );
    runner
        .state_mut()
        .objects
        .get_mut(&foreign)
        .unwrap()
        .controller = P0;
    runner
        .state_mut()
        .objects
        .get_mut(&foreign)
        .unwrap()
        .base_controller = Some(P0);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&foreign].power,
            runner.state().objects[&foreign].toughness
        ),
        (Some(4), Some(4))
    );
    assert!(runner.state().objects[&foreign]
        .keywords
        .contains(&Keyword::Flying));
    attach_to(runner.state_mut(), aura, archon);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&archon].power,
            runner.state().objects[&archon].toughness
        ),
        (Some(4), Some(4))
    );
    assert_eq!(
        (
            runner.state().objects[&foreign].power,
            runner.state().objects[&foreign].toughness
        ),
        (Some(2), Some(3))
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

#[test]
fn balthor_paid_activation_returns_each_owners_complete_color_union_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let balthor = scenario
        .add_creature_from_oracle(P0, "Balthor the Defiled", 2, 2, BALTHOR)
        .with_subtypes(vec!["Zombie", "Dwarf"])
        .id();
    let minion = scenario
        .add_creature(P1, "Independent Minion", 1, 1)
        .with_subtypes(vec!["Minion"])
        .id();
    let mut returned = Vec::new();
    let mut excluded = Vec::new();
    let red_in_hand = scenario
        .add_creature_to_hand(P0, "Red foreign-zone decoy", 2, 2)
        .with_color(vec![ManaColor::Red])
        .id();
    for owner in [P0, P1] {
        for (name, colors) in [
            ("Black creature", vec![ManaColor::Black]),
            ("Red creature", vec![ManaColor::Red]),
            ("Both colors", vec![ManaColor::Black, ManaColor::Red]),
        ] {
            returned.push((
                owner,
                scenario
                    .add_creature_to_graveyard(owner, name, 2, 2)
                    .with_color(colors)
                    .controlled_by(if owner == P0 { P1 } else { P0 })
                    .id(),
            ));
        }
        excluded.push(
            scenario
                .add_creature_to_graveyard(owner, "Blue creature", 2, 2)
                .with_color(vec![ManaColor::Blue])
                .id(),
        );
        excluded.push(
            scenario
                .add_spell_to_graveyard(owner, "Black noncreature", true)
                .with_color(vec![ManaColor::Black])
                .id(),
        );
    }
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&minion].power, Some(2));
    let outcome = runner.activate(balthor, 0).resolve();
    outcome.assert_zone(&[balthor], Zone::Exile);
    outcome.assert_zone(&excluded, Zone::Graveyard);
    outcome.assert_zone(&[red_in_hand], Zone::Hand);
    outcome.assert_power_toughness(minion, 1, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    for (owner, id) in returned {
        outcome.assert_zone(&[id], Zone::Battlefield);
        assert_eq!(outcome.state().objects[&id].owner, owner);
        assert_eq!(outcome.state().objects[&id].controller, owner);
        assert_eq!(
            outcome
                .state()
                .battlefield
                .iter()
                .filter(|&&candidate| candidate == id)
                .count(),
            1
        );
    }
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
}

#[test]
fn balthor_empty_graveyards_still_pay_and_complete_the_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let balthor = scenario
        .add_creature_from_oracle(P0, "Balthor the Defiled", 2, 2, BALTHOR)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.activate(balthor, 0).resolve();
    outcome.assert_zone(&[balthor], Zone::Exile);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert!(outcome.state().battlefield.is_empty());
    assert!(outcome
        .state()
        .players
        .iter()
        .all(|player| player.graveyard.is_empty()));
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
}

#[test]
fn constructed_archon_other_exclusion_and_source_departure_are_observable() {
    // The printed Archon already is 4/4, so setting it to 4/4 would mask a
    // missing "other" restriction. This secondary constructed source gives
    // the same complete rules body distinguishable copiable characteristics.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Synthetic Archon restriction witness", 1, 2, ARCHON)
        .id();
    let own = scenario
        .add_creature(P0, "Own enchanted witness", 2, 3)
        .id();
    let aura = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic creature Aura", false, "Enchant creature")
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(aura).target_object(source).resolve();
    outcome.assert_power_toughness(source, 1, 2);
    assert!(outcome.state().objects[&source]
        .keywords
        .contains(&Keyword::Flying));
    attach_to(runner.state_mut(), aura, own);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&own].power,
            runner.state().objects[&own].toughness
        ),
        (Some(4), Some(4))
    );
    let outcome = runner.cast(bolt).target_object(source).resolve();
    outcome.assert_zone(&[source], Zone::Graveyard);
    outcome.assert_power_toughness(own, 2, 3);
    assert!(!outcome.state().objects[&own]
        .keywords
        .contains(&Keyword::Flying));
    outcome.assert_zone(&[aura], Zone::Battlefield);
    assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
}

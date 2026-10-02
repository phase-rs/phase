//! CR 106.1 + CR 701.20a: Metalworker activated mana ability:
//! "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for each card revealed this way."
//!
//! Verifies:
//! 1. Activating Metalworker with artifact cards in hand enters `WaitingFor::RevealChoice`
//!    with `any_number: true` and `optional: true`.
//! 2. Revealing 2 artifact cards produces 4 colorless mana ({C}{C}{C}{C}) and emits `CardsRevealed`.
//! 3. Revealing 1 artifact card produces 2 colorless mana ({C}{C}).
//! 4. Declining / revealing 0 cards produces 0 mana without error.
//! 5. Activating with 0 artifact cards in hand completes immediately with 0 mana.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const METALWORKER_ORACLE: &str =
    "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for each card revealed this way.";
const KINNAN_ORACLE: &str =
    "Whenever you tap a nonland permanent for mana, add one mana of any type that permanent produced.";

#[test]
fn metalworker_reveal_two_artifacts_adds_four_colorless_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let art1 = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let art2 = scenario
        .add_creature_to_hand(P0, "Memnite", 1, 1)
        .as_artifact()
        .id();

    let non_art = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();

    let mut runner = scenario.build();

    // Activating Metalworker's {T} ability
    let result = runner.act(GameAction::ActivateAbility {
        source_id: metalworker,
        ability_index: 0,
    });
    assert!(result.is_ok(), "activation succeeds: {result:?}");

    // Engine should be waiting for reveal choice with any_number: true
    match runner.state().waiting_for {
        WaitingFor::RevealChoice {
            player,
            ref cards,
            optional,
            any_number,
            ..
        } => {
            assert_eq!(player, P0);
            assert!(optional);
            assert!(any_number);
            assert!(cards.contains(&art1));
            assert!(cards.contains(&art2));
            assert!(!cards.contains(&non_art));
        }
        ref wf => panic!("expected WaitingFor::RevealChoice, got {:?}", wf),
    }

    // Player chooses to reveal both artifact cards
    let result = runner.act(GameAction::SelectCards {
        cards: vec![art1, art2],
    });
    assert!(result.is_ok(), "selection succeeds: {result:?}");
    let events = result.unwrap().events;

    // Mana pool should have 4 colorless mana
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        4,
        "expected 4 colorless mana from 2 revealed artifacts"
    );

    // CardsRevealed event should have been emitted
    let revealed_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed {
                player, card_ids, ..
            } => Some((*player, card_ids.clone())),
            _ => None,
        })
        .collect();
    assert!(
        revealed_events
            .iter()
            .any(|(p, c)| *p == P0 && c.contains(&art1) && c.contains(&art2)),
        "expected CardsRevealed event containing both artifact cards, got {revealed_events:?}"
    );
}

#[test]
fn metalworker_reveal_one_artifact_adds_two_colorless_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let art1 = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let _art2 = scenario
        .add_creature_to_hand(P0, "Memnite", 1, 1)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("activation succeeds");

    // Choose to reveal only 1 artifact
    runner
        .act(GameAction::SelectCards { cards: vec![art1] })
        .expect("selection succeeds");

    // Mana pool should have 2 colorless mana
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        2,
        "expected 2 colorless mana from 1 revealed artifact"
    );
}

#[test]
fn metalworker_reveal_zero_artifacts_by_selecting_empty_adds_zero_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let _art1 = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("activation succeeds");

    // Choose to reveal 0 cards
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("empty selection succeeds");

    // Mana pool should have 0 colorless mana
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "expected 0 colorless mana when revealing 0 cards"
    );
}

#[test]
fn metalworker_with_no_artifacts_in_hand_resolves_immediately_with_zero_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let _non_art = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("activation succeeds");

    // Should NOT be waiting for reveal choice since no eligible cards exist
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::RevealChoice { .. }),
        "should not prompt for reveal choice when hand has 0 artifacts"
    );

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "expected 0 colorless mana"
    );
}

#[test]
fn metalworker_with_kinnan_produces_three_colorless_and_one_tapped_for_mana_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let _kinnan = scenario
        .add_creature_from_oracle(P0, "Kinnan, Bonder Prodigy", 2, 2, KINNAN_ORACLE)
        .id();

    let art1 = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    let result = runner.act(GameAction::ActivateAbility {
        source_id: metalworker,
        ability_index: 0,
    });
    assert!(result.is_ok(), "activation succeeds: {result:?}");

    let result = runner.act(GameAction::SelectCards { cards: vec![art1] });
    assert!(result.is_ok(), "reveal selection succeeds: {result:?}");
    let outcome = result.unwrap();

    // 1 artifact revealed -> Metalworker produces {C}{C}, Kinnan adds {C} -> 3 total colorless mana
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        3,
        "expected 3 colorless mana ({{C}}{{C}} from Metalworker + {{C}} from Kinnan)"
    );

    // Exactly one TappedForMana event emitted for Metalworker's aggregate activation
    let tapped_for_mana_events: Vec<_> = outcome
        .events
        .iter()
        .filter(
            |e| matches!(e, GameEvent::TappedForMana { source_id, .. } if *source_id == metalworker),
        )
        .collect();
    assert_eq!(
        tapped_for_mana_events.len(),
        1,
        "expected exactly one TappedForMana event for Metalworker activation, got {tapped_for_mana_events:?}"
    );

    let mana_produced_events: Vec<_> = outcome
        .events
        .iter()
        .filter(|e| {
            matches!(e, GameEvent::ManaAbilityProduced { source_id, .. } if *source_id == metalworker)
        })
        .collect();
    assert_eq!(
        mana_produced_events.len(),
        1,
        "expected exactly one ManaAbilityProduced event for Metalworker activation, got {mana_produced_events:?}"
    );
}

#[test]
fn metalworker_activated_during_spell_mana_payment_resumes_and_finalizes_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let art1 = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let art2 = scenario
        .add_creature_to_hand(P0, "Memnite", 1, 1)
        .as_artifact()
        .id();

    let spell = scenario
        .add_spell_to_hand(P0, "Juggernaut", true)
        .with_mana_cost(ManaCost::generic(4))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    // Begin casting the spell in manual payment mode
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual cast begins and reaches mana payment prompt");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "expected WaitingFor::ManaPayment, got {:?}",
        runner.state().waiting_for
    );

    // Activate Metalworker during spell mana payment
    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("Metalworker activation during spell payment succeeds");

    // Engine pauses at WaitingFor::RevealChoice with pending_mana_ability preserving payment resume
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::RevealChoice { .. }),
        "expected WaitingFor::RevealChoice, got {:?}",
        runner.state().waiting_for
    );

    // Reveal 2 artifacts to produce 4 colorless mana
    runner
        .act(GameAction::SelectCards {
            cards: vec![art1, art2],
        })
        .expect("reveal selection succeeds");

    // Engine must have resumed exactly to the spell's mana payment context
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "expected return to WaitingFor::ManaPayment, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        4,
        "expected 4 colorless mana in pool from Metalworker"
    );

    // Finalize payment / cast the spell
    runner
        .act(GameAction::PassPriority)
        .expect("spell mana payment succeeds and cast completes");

    // Spell is now on the stack
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

#[test]
fn metalworker_independent_sequential_activations_isolate_tracked_sets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();

    let art1 = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let art2 = scenario
        .add_creature_to_hand(P0, "Memnite", 1, 1)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    // 1st activation: reveal Art1 -> produces {C}{C} (pool = 2)
    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("1st activation succeeds");
    runner
        .act(GameAction::SelectCards { cards: vec![art1] })
        .expect("reveal art1 succeeds");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        2,
        "expected 2 colorless mana after revealing 1st artifact"
    );

    // Untap Metalworker for 2nd independent activation
    runner
        .state_mut()
        .objects
        .get_mut(&metalworker)
        .unwrap()
        .tapped = false;

    // 2nd activation: choose empty (0 cards) -> produces 0 mana (pool remains 2)
    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("2nd activation succeeds");
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("empty reveal succeeds");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        2,
        "expected pool to remain 2 after choosing 0 cards (not retaining prior activation's set)"
    );

    // Untap Metalworker for 3rd independent activation
    runner
        .state_mut()
        .objects
        .get_mut(&metalworker)
        .unwrap()
        .tapped = false;

    // 3rd activation: reveal Art2 -> produces {C}{C} (pool = 2 + 2 = 4)
    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
        })
        .expect("3rd activation succeeds");
    runner
        .act(GameAction::SelectCards { cards: vec![art2] })
        .expect("reveal art2 succeeds");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        4,
        "expected 4 total colorless mana after 3rd activation revealing 1 artifact (not unioned with art1)"
    );
}

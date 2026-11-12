//! CR 605.1a + CR 605.3b + CR 107.1c + CR 701.20a: "reveal any number of
//! [filter] cards in your hand" through production entry points.
//!
//! Metalworker (verified Oracle text, Scryfall): "{T}: Reveal any number of
//! artifact cards in your hand. Add {C}{C} for each card revealed this way." —
//! a mana ability whose reveal is a resolution-time selection of zero or more
//! matching cards, whose mana scales with the cards actually revealed.
//!
//! Scent of Jasmine (verified Oracle text, Scryfall): "Reveal any number of white
//! cards in your hand. You gain 2 life for each card revealed this way." — the
//! same selection resolving from the stack.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{ObjectSelectionCardinality, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const METALWORKER_ORACLE: &str =
    "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for each card revealed this way.";
const KINNAN_ORACLE: &str =
    "Whenever you tap a nonland permanent for mana, add one mana of any type that permanent produced.";
const EMPOWERED_AUTOGENERATOR_ORACLE: &str = "{T}: Put a charge counter on this artifact. Add X mana of any one color, where X is the number of charge counters on this artifact.";
const SCENT_OF_JASMINE_ORACLE: &str =
    "Reveal any number of white cards in your hand. You gain 2 life for each card revealed this way.";

/// Metalworker on P0's battlefield plus `artifacts` artifact cards and one
/// non-artifact card in P0's hand. Returns the runner, Metalworker, the artifact
/// ids, and the non-artifact id.
fn metalworker_board(artifacts: usize) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();
    let artifact_ids = (0..artifacts)
        .map(|i| {
            scenario
                .add_creature_to_hand(P0, &format!("Artifact {i}"), 1, 1)
                .as_artifact()
                .id()
        })
        .collect();
    let non_artifact = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    (scenario.build(), metalworker, artifact_ids, non_artifact)
}

fn colorless(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(ManaType::Colorless)
}

/// Positive completion of a Metalworker activation: P0 holds priority, nothing
/// is suspended on the stack, and the {T} cost was paid.
fn assert_activation_completed(runner: &GameRunner, metalworker: ObjectId) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the activation must complete back to P0's priority, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "a mana ability never uses the stack"
    );
    assert!(
        state.objects[&metalworker].tapped,
        "the {{T}} activation cost must be paid"
    );
}

fn activate(runner: &mut GameRunner, metalworker: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Metalworker activation is legal");
}

fn revealed_ids(events: &[GameEvent]) -> Vec<Vec<ObjectId>> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn metalworker_prompts_any_number_and_two_revealed_artifacts_add_four_colorless() {
    let (mut runner, metalworker, artifacts, non_artifact) = metalworker_board(2);
    activate(&mut runner, metalworker);

    match &runner.state().waiting_for {
        WaitingFor::RevealChoice {
            player,
            cards,
            cardinality,
            pending_mana_ability,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*cardinality, ObjectSelectionCardinality::AnyNumber);
            assert!(pending_mana_ability.is_some(), "the mana ability resumes");
            assert_eq!(cards, &artifacts, "only artifact cards are eligible");
            assert!(!cards.contains(&non_artifact));
        }
        other => panic!("expected an any-number RevealChoice, got {other:?}"),
    }
    // CR 605.3b: the selection is part of the mana ability's immediate
    // resolution, so the prompt continues that activation.
    assert!(
        runner.state().waiting_for.is_mana_ability_continuation(),
        "the reveal prompt continues the mana ability's activation"
    );
    assert!(
        runner.state().withholds_priority(),
        "no player receives priority while the activation's reveal is pending"
    );

    let outcome = runner
        .act(GameAction::SelectCards {
            cards: artifacts.clone(),
        })
        .expect("revealing both artifacts is legal");

    assert_eq!(colorless(&runner), 4, "{{C}}{{C}} for each of two cards");
    assert_eq!(revealed_ids(&outcome.events), vec![artifacts]);
    assert_activation_completed(&runner, metalworker);
    assert!(
        !runner.state().waiting_for.is_mana_ability_continuation(),
        "the continuation is cleared once the activation completes"
    );
    assert!(!runner.state().withholds_priority());
}

#[test]
fn metalworker_one_revealed_artifact_adds_two_colorless() {
    let (mut runner, metalworker, artifacts, _) = metalworker_board(2);
    activate(&mut runner, metalworker);
    runner
        .act(GameAction::SelectCards {
            cards: vec![artifacts[0]],
        })
        .expect("revealing one artifact is legal");

    assert_eq!(colorless(&runner), 2);
    assert_activation_completed(&runner, metalworker);
}

#[test]
fn metalworker_revealing_zero_cards_completes_with_no_mana() {
    let (mut runner, metalworker, _, _) = metalworker_board(1);
    activate(&mut runner, metalworker);
    let outcome = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("CR 107.1c: zero is a legal any-number choice");

    assert_eq!(colorless(&runner), 0);
    assert!(
        revealed_ids(&outcome.events).is_empty(),
        "nothing is revealed"
    );
    assert_activation_completed(&runner, metalworker);
}

#[test]
fn metalworker_with_no_artifacts_in_hand_completes_without_a_prompt() {
    let (mut runner, metalworker, _, _) = metalworker_board(0);
    let outcome = runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is legal with no artifacts in hand");

    assert_eq!(colorless(&runner), 0);
    assert!(revealed_ids(&outcome.events).is_empty());
    assert_activation_completed(&runner, metalworker);
}

#[test]
fn metalworker_rejects_a_repeated_card_before_revealing_anything() {
    let (mut runner, metalworker, artifacts, _) = metalworker_board(2);
    activate(&mut runner, metalworker);

    let rejected = runner.act(GameAction::SelectCards {
        cards: vec![artifacts[0], artifacts[0]],
    });
    assert!(
        rejected.is_err(),
        "a repeated card must be rejected, got {rejected:?}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::RevealChoice { .. }),
        "the prompt stays open after a rejected selection"
    );
    assert_eq!(colorless(&runner), 0, "no mana from a rejected selection");

    // Reach-guard: the same prompt accepts the distinct selection.
    runner
        .act(GameAction::SelectCards {
            cards: artifacts.clone(),
        })
        .expect("a distinct selection is legal");
    assert_eq!(colorless(&runner), 4);
    assert_activation_completed(&runner, metalworker);
}

#[test]
fn metalworker_prompt_issues_a_legal_multi_card_action() {
    let (mut runner, metalworker, artifacts, _) = metalworker_board(2);
    activate(&mut runner, metalworker);

    let both = GameAction::SelectCards {
        cards: artifacts.clone(),
    };
    let issued = legal_actions(runner.state());
    assert!(
        issued.contains(&both),
        "the any-number prompt must issue revealing every eligible card, got {issued:?}"
    );
    assert!(
        issued.contains(&GameAction::SelectCards { cards: vec![] }),
        "CR 107.1c: revealing zero cards is issued too, got {issued:?}"
    );

    runner.act(both).expect("the issued action is accepted");
    assert_eq!(colorless(&runner), 4);
    assert_activation_completed(&runner, metalworker);
}

#[test]
fn metalworker_with_kinnan_adds_three_colorless_from_one_mana_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();
    scenario.add_creature_from_oracle(P0, "Kinnan, Bonder Prodigy", 2, 2, KINNAN_ORACLE);
    let artifact = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    activate(&mut runner, metalworker);
    let outcome = runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .expect("reveal selection is legal");

    // {C}{C} from Metalworker plus Kinnan's one added {C}.
    assert_eq!(colorless(&runner), 3);
    let tapped_for_mana = outcome
        .events
        .iter()
        .filter(|e| matches!(e, GameEvent::TappedForMana { source_id, .. } if *source_id == metalworker))
        .count();
    assert_eq!(tapped_for_mana, 1, "one TappedForMana for the activation");
    let produced = outcome
        .events
        .iter()
        .filter(|e| {
            matches!(e, GameEvent::ManaAbilityProduced { source_id, .. } if *source_id == metalworker)
        })
        .count();
    assert_eq!(produced, 1, "one ManaAbilityProduced for the activation");
    assert_activation_completed(&runner, metalworker);
}

#[test]
fn metalworker_activated_during_spell_payment_resumes_and_finalizes_the_cast() {
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

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual cast reaches the mana payment prompt");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "expected ManaPayment, got {:?}",
        runner.state().waiting_for
    );

    activate(&mut runner, metalworker);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RevealChoice {
                pending_mana_ability: Some(_),
                ..
            }
        ),
        "expected the mana ability's RevealChoice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![art1, art2],
        })
        .expect("reveal selection is legal");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the activation resumes the spell's payment, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(colorless(&runner), 4);
    assert!(runner.state().objects[&metalworker].tapped);

    runner
        .act(GameAction::PassPriority)
        .expect("the pooled mana pays the spell and the cast completes");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes to P0's priority, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn metalworker_sequential_activations_each_count_only_their_own_reveal() {
    let (mut runner, metalworker, artifacts, _) = metalworker_board(2);
    let untap = |runner: &mut GameRunner| {
        runner
            .state_mut()
            .objects
            .get_mut(&metalworker)
            .unwrap()
            .tapped = false;
    };

    activate(&mut runner, metalworker);
    runner
        .act(GameAction::SelectCards {
            cards: vec![artifacts[0]],
        })
        .expect("first reveal is legal");
    assert_eq!(colorless(&runner), 2);
    assert_activation_completed(&runner, metalworker);

    // An empty reveal must not reuse the first activation's revealed set.
    untap(&mut runner);
    activate(&mut runner, metalworker);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("empty reveal is legal");
    assert_eq!(colorless(&runner), 2);
    assert_activation_completed(&runner, metalworker);

    // A later reveal counts only its own card, not a union with the first.
    untap(&mut runner);
    activate(&mut runner, metalworker);
    runner
        .act(GameAction::SelectCards {
            cards: vec![artifacts[1]],
        })
        .expect("third reveal is legal");
    assert_eq!(colorless(&runner), 4);
    assert_activation_completed(&runner, metalworker);
}

/// The same any-number selection resolving from the stack: only the chosen
/// white cards are revealed, a repeated card is rejected, and the life gain
/// counts the cards revealed this way.
#[test]
fn scent_of_jasmine_reveals_any_number_of_white_cards_from_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scent = scenario
        .add_spell_to_hand_from_oracle(P0, "Scent of Jasmine", true, SCENT_OF_JASMINE_ORACLE)
        .id();
    let white: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature_to_hand(P0, &format!("White {i}"), 1, 1)
                .with_color(vec![ManaColor::White])
                .id()
        })
        .collect();
    let green = scenario
        .add_creature_to_hand(P0, "Green", 1, 1)
        .with_color(vec![ManaColor::Green])
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let cast = runner.cast(scent).resolve();
    match cast.final_waiting_for() {
        WaitingFor::RevealChoice {
            player,
            cards,
            filter,
            cardinality,
            pending_mana_ability,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*cardinality, ObjectSelectionCardinality::AnyNumber);
            assert!(pending_mana_ability.is_none());
            assert!(matches!(filter, TargetFilter::Typed(_)));
            assert_eq!(cards, &white, "only white cards are eligible");
            assert!(!cards.contains(&green));
        }
        other => panic!("expected an any-number RevealChoice, got {other:?}"),
    }

    let rejected = runner.act(GameAction::SelectCards {
        cards: vec![white[0], white[0]],
    });
    assert!(rejected.is_err(), "a repeated card must be rejected");

    let chosen = vec![white[0], white[2]];
    let outcome = runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("a distinct any-number selection is legal");
    assert_eq!(revealed_ids(&outcome.events), vec![chosen]);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 4,
        "2 life for each of the two cards revealed this way"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the spell resolves to P0's priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the spell finished resolving"
    );
}

/// A later activation with no eligible card must count its own (empty) reveal,
/// never the set an earlier activation revealed. CR 107.1c: with nothing
/// eligible, zero cards are revealed and no mana is added.
#[test]
fn metalworker_with_nothing_eligible_ignores_an_earlier_activations_reveal() {
    let (mut runner, metalworker, artifacts, _) = metalworker_board(1);
    activate(&mut runner, metalworker);
    runner
        .act(GameAction::SelectCards {
            cards: artifacts.clone(),
        })
        .expect("first reveal is legal");
    assert_eq!(colorless(&runner), 2);

    // The revealed artifact leaves the hand (cast for {0}), so the next
    // activation has nothing eligible.
    runner.cast(artifacts[0]).resolve();
    assert_eq!(
        runner.state().objects[&artifacts[0]].zone,
        Zone::Battlefield
    );
    runner
        .state_mut()
        .objects
        .get_mut(&metalworker)
        .unwrap()
        .tapped = false;

    let outcome = runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is legal with nothing eligible");
    assert!(revealed_ids(&outcome.events).is_empty());
    assert_eq!(
        colorless(&runner),
        2,
        "zero cards revealed this way adds no mana"
    );
    assert_activation_completed(&runner, metalworker);
}

/// CR 608.2c: Empowered Autogenerator (verified Oracle text, Scryfall) puts its
/// charge counter on BEFORE its mana is counted, and its "any one color" is the
/// controller's choice. Its production follows a non-Mana clause, so it
/// resolves in written order on the stack rather than through the immediate
/// mana-ability resolver (which would produce first and read zero counters).
#[test]
fn empowered_autogenerator_counts_the_counter_it_just_placed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let autogenerator = scenario
        .add_artifact_from_oracle(
            P0,
            "Empowered Autogenerator",
            EMPOWERED_AUTOGENERATOR_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    let charge = CounterType::Generic("charge".to_string());

    let tap_for = |runner: &mut GameRunner, color: ManaType| {
        runner
            .state_mut()
            .objects
            .get_mut(&autogenerator)
            .unwrap()
            .tapped = false;
        runner
            .act(GameAction::ActivateAbility {
                source_id: autogenerator,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activation is legal");
        while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !runner.state().stack.is_empty()
        {
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority resolves the ability");
        }
        match &runner.state().waiting_for {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => assert!(
                options.contains(&color),
                "any one color offers {color:?}, got {options:?}"
            ),
            other => panic!("expected the any-one-color prompt, got {other:?}"),
        }
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(color),
                count: 1,
            })
            .expect("choosing the color is legal");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
            "the ability finishes resolving, got {:?}",
            runner.state().waiting_for
        );
        assert!(runner.state().stack.is_empty());
    };

    tap_for(&mut runner, ManaType::Blue);
    assert_eq!(runner.state().objects[&autogenerator].counters[&charge], 1);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        1,
        "first tap: one counter, so one mana of the chosen color"
    );

    tap_for(&mut runner, ManaType::Red);
    assert_eq!(runner.state().objects[&autogenerator].counters[&charge], 2);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        2,
        "second tap: two counters, so two mana of the chosen color"
    );
}

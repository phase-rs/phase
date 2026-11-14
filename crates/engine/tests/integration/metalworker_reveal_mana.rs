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

/// CR 400.2 + CR 107.1c: hand size is public but eligibility is not, so a
/// non-empty hand with nothing eligible still gets the prompt; its only legal
/// answer is the empty selection, which adds no mana.
#[test]
fn metalworker_with_no_artifacts_in_non_empty_hand_prompts_and_only_empty_is_legal() {
    let (mut runner, metalworker, _, non_artifact) = metalworker_board(0);
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
            assert!(cards.is_empty(), "nothing is eligible");
            assert!(pending_mana_ability.is_some());
        }
        other => panic!("expected a RevealChoice even with nothing eligible, got {other:?}"),
    }
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![non_artifact],
            })
            .is_err(),
        "an ineligible card cannot be revealed"
    );
    let issued = legal_actions(runner.state());
    assert!(
        issued.contains(&GameAction::SelectCards { cards: vec![] }),
        "the empty selection is issued, got {issued:?}"
    );
    let outcome = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("the empty selection is legal");

    assert_eq!(colorless(&runner), 0);
    assert!(revealed_ids(&outcome.events).is_empty());
    assert_activation_completed(&runner, metalworker);
}

/// CR 400.2: only an empty hand (public) skips the prompt.
#[test]
fn metalworker_with_an_empty_hand_completes_without_a_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .act(GameAction::ActivateAbility {
            source_id: metalworker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is legal with an empty hand");

    assert_eq!(colorless(&runner), 0);
    assert!(revealed_ids(&outcome.events).is_empty());
    assert_activation_completed(&runner, metalworker);
}

/// A board whose two hand cards are identical in every public respect; only
/// the first's eligibility (artifact vs. creature) differs.
fn two_card_hand_board(first_is_artifact: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();
    let mut first = scenario.add_creature_to_hand(P0, "Hidden A", 1, 1);
    if first_is_artifact {
        first.as_artifact();
    }
    scenario.add_creature_to_hand(P0, "Hidden B", 2, 2);
    (scenario.build(), metalworker)
}

/// CR 400.2 + CR 701.20a: the pre-reveal prompt's projection to an opponent
/// and to a spectator is byte-identical whether or not an unrevealed hand card
/// is eligible, while the owner still sees the full options.
#[test]
fn metalworker_pre_reveal_projection_is_independent_of_hidden_eligibility() {
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    use engine::types::player::PlayerId;

    let (mut eligible, mw_a) = two_card_hand_board(true);
    let (mut ineligible, mw_b) = two_card_hand_board(false);
    activate(&mut eligible, mw_a);
    activate(&mut ineligible, mw_b);

    // The owner sees the real candidate list in the eligible world only.
    for (runner, expected_len) in [(&eligible, 1usize), (&ineligible, 0usize)] {
        match &filter_state_for_viewer(runner.state(), P0).waiting_for {
            WaitingFor::RevealChoice {
                cards,
                pending_mana_ability,
                ..
            } => {
                assert_eq!(cards.len(), expected_len, "owner keeps full options");
                assert!(pending_mana_ability.is_some());
            }
            other => panic!("owner must see the RevealChoice, got {other:?}"),
        }
    }

    let opponent = PlayerId(1);
    let json = |state| serde_json::to_string(&state).expect("projection serializes");
    assert_eq!(
        json(filter_state_for_viewer(eligible.state(), opponent)),
        json(filter_state_for_viewer(ineligible.state(), opponent)),
        "opponent projection must not encode private eligibility"
    );
    assert_eq!(
        json(filter_state_for_unseated_viewer(eligible.state())),
        json(filter_state_for_unseated_viewer(ineligible.state())),
        "spectator projection must not encode private eligibility"
    );
    match &filter_state_for_viewer(eligible.state(), opponent).waiting_for {
        WaitingFor::RevealChoice {
            cards,
            pending_mana_ability,
            ..
        } => {
            assert!(cards.is_empty());
            assert!(pending_mana_ability.is_none());
        }
        other => panic!("opponent still sees that a reveal is pending, got {other:?}"),
    }
}

/// CR 400.2: the stack-resolution producer follows the same rule — a
/// non-empty hand with no white card still prompts, and only empty is legal.
#[test]
fn scent_of_jasmine_with_nothing_eligible_in_non_empty_hand_prompts_for_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scent = scenario
        .add_spell_to_hand_from_oracle(P0, "Scent of Jasmine", true, SCENT_OF_JASMINE_ORACLE)
        .id();
    let green = scenario
        .add_creature_to_hand(P0, "Green", 1, 1)
        .with_color(vec![ManaColor::Green])
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let cast = runner.cast(scent).resolve();
    match cast.final_waiting_for() {
        WaitingFor::RevealChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert!(cards.is_empty());
        }
        other => panic!("expected a RevealChoice with nothing eligible, got {other:?}"),
    }
    assert!(runner
        .act(GameAction::SelectCards { cards: vec![green] })
        .is_err());
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("the empty selection is legal");
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);
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

    // CR 400.2: the hand still holds a non-artifact card, so the prompt appears
    // and only the empty selection is legal.
    activate(&mut runner, metalworker);
    let outcome = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("the empty selection is legal with nothing eligible");
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
/// CR 605.1a + CR 605.3b + CR 608.2c: Empowered Autogenerator's "Put a charge
/// counter on this artifact. Add X mana of any one color, where X is the number
/// of charge counters on this artifact." is a mana ability. Activated while a
/// spell's mana is being paid it resolves at once and in written order: the
/// counter is placed before the colour prompt, nothing uses the stack, X counts
/// the counter just placed, and the suspended payment resumes and completes.
fn autogenerator_pays_spell(prior_counters: u32, spell_cost: u32) {
    let charge = CounterType::Generic("charge".to_string());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let autogenerator = scenario
        .add_artifact_from_oracle(
            P0,
            "Empowered Autogenerator",
            EMPOWERED_AUTOGENERATOR_ORACLE,
        )
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Juggernaut", true)
        .with_mana_cost(ManaCost::generic(spell_cost))
        .id();
    let mut runner = scenario.build();
    if prior_counters > 0 {
        runner
            .state_mut()
            .objects
            .get_mut(&autogenerator)
            .unwrap()
            .counters
            .insert(charge.clone(), prior_counters);
    }
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

    runner
        .act(GameAction::ActivateAbility {
            source_id: autogenerator,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is legal during payment");

    // The colour prompt is reached WITHOUT passing priority, the counter was
    // placed first, and no ability sits on the stack (only the spell being cast).
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => assert!(options.contains(&ManaType::Blue), "got {options:?}"),
        other => panic!("expected the any-one-color prompt, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&autogenerator].counters[&charge],
        prior_counters + 1,
        "the counter is placed before the colour choice"
    );
    assert!(
        runner.state().stack.iter().all(|entry| matches!(
            entry.kind,
            engine::types::game_state::StackEntryKind::Spell { .. }
        )),
        "a mana ability never uses the stack: {:?}",
        runner.state().stack
    );

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("choosing the color is legal");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue) as u32,
        prior_counters + 1,
        "X is the number of charge counters after the new one"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the suspended payment resumes, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PassPriority)
        .expect("the pooled mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes to P0's priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().pending_cast.is_none(), "payment finalized");
}

#[test]
fn empowered_autogenerator_first_activation_counts_the_counter_it_just_placed() {
    autogenerator_pays_spell(0, 1);
}

#[test]
fn empowered_autogenerator_second_activation_adds_two_mana() {
    autogenerator_pays_spell(1, 2);
}

/// Walking Ballista (verified Oracle text, Scryfall): {X}{X} artifact creature.
/// P0 controls an untapped Metalworker and holds `others` other artifact cards
/// (plus the Ballista), with no other mana. Returns the runner after casting
/// has reached the X announcement and the announced maximum.
fn cast_walking_ballista(others: usize) -> (GameRunner, ObjectId, Vec<ObjectId>, u32) {
    let (runner, metalworker, artifacts, _ballista, max) =
        cast_walking_ballista_with(others, CastPaymentMode::Auto);
    (runner, metalworker, artifacts, max)
}

fn cast_walking_ballista_with(
    others: usize,
    payment_mode: CastPaymentMode,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, u32) {
    use engine::types::mana::ManaCostShard;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let metalworker = scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact()
        .id();
    let artifacts: Vec<ObjectId> = (0..others)
        .map(|i| {
            scenario
                .add_creature_to_hand(P0, &format!("Artifact {i}"), 1, 1)
                .as_artifact()
                .id()
        })
        .collect();
    let ballista = scenario
        .add_creature_to_hand(P0, "Walking Ballista", 0, 0)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&ballista].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ballista,
            card_id,
            targets: Vec::new(),
            payment_mode,
        })
        .expect("casting reaches the X announcement");
    let max = match &runner.state().waiting_for {
        WaitingFor::ChooseXValue { max, .. } => *max,
        other => panic!("expected the X announcement, got {other:?}"),
    };
    (runner, metalworker, artifacts, ballista, max)
}

/// CR 605.3a + CR 107.1c: a Metalworker that could reveal both other artifact
/// cards in hand contributes {C}{C} for each, so {X}{X} can announce X=2 and be
/// paid by revealing them while paying.
#[test]
fn metalworker_capacity_counts_the_artifacts_it_could_reveal_for_announced_x() {
    let (mut runner, metalworker, artifacts, ballista, max) =
        cast_walking_ballista_with(2, CastPaymentMode::Auto);
    assert_eq!(
        max, 2,
        "four colorless from revealing two artifacts pays X=2"
    );

    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announcing X=2 is legal");
    // Auto cannot choose the reveal itself: payment is the payer's, with nothing
    // yet revealed or pooled.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "expected the payer's ManaPayment, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(colorless(&runner), 0);

    // Pay by revealing both artifact cards through the mana ability.
    activate(&mut runner, metalworker);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::RevealChoice { .. }),
        "expected the payer's RevealChoice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: artifacts.clone(),
        })
        .expect("revealing both artifacts is legal");
    assert_eq!(colorless(&runner), 4);
    runner.act(GameAction::PassPriority).expect("pay");
    // CR 601.2h: payment finalized — the spell is on the stack, the cast is no
    // longer pending, and the caster holds priority again.
    assert_eq!(runner.state().objects[&ballista].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none(), "payment finalized");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes to P0's priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Control: one other artifact card in hand caps X at 1.
#[test]
fn metalworker_capacity_with_one_artifact_caps_announced_x_at_one() {
    let (_runner, _metalworker, _artifacts, max) = cast_walking_ballista(1);
    assert_eq!(max, 1, "two colorless from revealing one artifact pays X=1");
}

/// CR 601.2a: the spell being cast has already been moved to the stack when its
/// costs are paid, so the live reveal prompt must not offer it as a card in hand.
#[test]
fn metalworker_reveal_prompt_excludes_the_spell_being_cast() {
    let (mut runner, metalworker, artifacts, ballista, max) =
        cast_walking_ballista_with(1, CastPaymentMode::Manual);
    assert_eq!(max, 1);
    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("announcing X=1 is legal");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "expected ManaPayment, got {:?}",
        runner.state().waiting_for
    );
    activate(&mut runner, metalworker);
    match &runner.state().waiting_for {
        WaitingFor::RevealChoice { cards, .. } => {
            assert!(
                !cards.contains(&ballista),
                "the spell being cast is not a card in hand: {cards:?}"
            );
            assert_eq!(cards, &artifacts, "the other artifact card is offered");
        }
        other => panic!("expected the mana ability's RevealChoice, got {other:?}"),
    }
}

/// Walking Ballista cast with Metalworker out and two other cards in hand, both
/// artifacts or neither. Returns the runner at the X announcement.
fn ballista_with_two_hidden_cards(artifacts: bool) -> GameRunner {
    use engine::types::mana::ManaCostShard;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Metalworker", 1, 2, METALWORKER_ORACLE)
        .as_artifact();
    for name in ["Hidden A", "Hidden B"] {
        let mut card = scenario.add_creature_to_hand(P0, name, 1, 1);
        if artifacts {
            card.as_artifact();
        }
    }
    let ballista = scenario
        .add_creature_to_hand(P0, "Walking Ballista", 0, 0)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&ballista].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ballista,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("casting reaches the X announcement");
    runner
}

/// CR 402.3 + CR 400.2: the unchosen X bound is computed from the payer's
/// private hand (Metalworker's capacity is per revealable artifact card), so an
/// opponent's and a spectator's projection of the X prompt is identical whether
/// the hidden hand holds artifacts or not; the payer still sees the real bound.
#[test]
fn x_announcement_projection_is_independent_of_private_hand_contents() {
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    use engine::types::player::PlayerId;

    let rich = ballista_with_two_hidden_cards(true);
    let poor = ballista_with_two_hidden_cards(false);

    let bound = |state: &engine::types::game_state::GameState| match &state.waiting_for {
        WaitingFor::ChooseXValue { max, .. } => *max,
        other => panic!("expected the X announcement, got {other:?}"),
    };
    assert_eq!(bound(&filter_state_for_viewer(rich.state(), P0)), 2);
    assert_eq!(bound(&filter_state_for_viewer(poor.state(), P0)), 0);

    let opponent = PlayerId(1);
    let json = |state| serde_json::to_string(&state).expect("projection serializes");
    assert_eq!(
        json(filter_state_for_viewer(rich.state(), opponent)),
        json(filter_state_for_viewer(poor.state(), opponent)),
        "opponent projection must not encode the payer's private hand"
    );
    assert_eq!(
        json(filter_state_for_unseated_viewer(rich.state())),
        json(filter_state_for_unseated_viewer(poor.state())),
        "spectator projection must not encode the payer's private hand"
    );
}

/// CR 601.2g + CR 605.3a: Metalworker plus two artifact cards in hand is the only
/// source able to pay Juggernaut's {4}. Auto payment cannot choose a reveal
/// itself, so it must hand the payment to the payer (manual payment) rather
/// than fail; the payer reveals, and the cast then completes.
#[test]
fn auto_cast_with_metalworker_as_only_source_reaches_the_payer_reveal() {
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
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Auto cast must not fail while Metalworker can pay");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "Auto hands the payment to the payer, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().objects[&art1].zone == Zone::Hand
            && runner.state().objects[&art2].zone == Zone::Hand,
        "Auto never reveals or moves cards itself"
    );

    activate(&mut runner, metalworker);
    match &runner.state().waiting_for {
        WaitingFor::RevealChoice { cards, .. } => {
            assert_eq!(cards.len(), 2, "both other artifacts are offered");
        }
        other => panic!("expected the payer's RevealChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![art1, art2],
        })
        .expect("reveal selection is legal");
    assert_eq!(colorless(&runner), 4);
    runner
        .act(GameAction::PassPriority)
        .expect("the pooled mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none(), "payment finalized");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the cast completes to P0's priority, got {:?}",
        runner.state().waiting_for
    );
}

const WINDING_CONSTRICTOR_ORACLE: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";
const DOUBLING_SEASON_ORACLE: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

/// CR 616.1 + CR 608.2c + CR 605.3b: Winding Constrictor ("plus one") and
/// Doubling Season (twice) both replace the charge counter Empowered
/// Autogenerator puts on itself. The controller orders them BEFORE the colour
/// prompt: adding first gives (1+1)x2 = 4 counters, doubling first gives
/// 1x2+1 = 3. The mana equals the resulting counter count, and the interrupted
/// cast's payment resumes exactly once and finalizes. Returns that count.
fn autogenerator_counter_order(order_index: usize) -> u32 {
    let charge = CounterType::Generic("charge".to_string());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let autogenerator = scenario
        .add_artifact_from_oracle(
            P0,
            "Empowered Autogenerator",
            EMPOWERED_AUTOGENERATOR_ORACLE,
        )
        .id();
    scenario.add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE);
    let spell = scenario
        .add_spell_to_hand(P0, "Juggernaut", true)
        .with_mana_cost(ManaCost::generic(3))
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
    runner
        .act(GameAction::ActivateAbility {
            source_id: autogenerator,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is legal during payment");

    // The replacement-order prompt comes first; nothing is on the stack but the
    // spell and no counter has landed yet.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected the CR 616.1 ordering prompt first, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.iter().all(|entry| matches!(
        entry.kind,
        engine::types::game_state::StackEntryKind::Spell { .. }
    )));
    runner
        .act(GameAction::ChooseReplacement { index: order_index })
        .expect("ordering choice is legal");

    // Then the colour prompt, with the counters already placed.
    let counters = runner.state().objects[&autogenerator]
        .counters
        .get(&charge)
        .copied()
        .unwrap_or(0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "expected the colour prompt after the ordering, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("choosing the color is legal");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue) as u32,
        counters,
        "X is the number of charge counters the ordered replacements left"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the interrupted payment resumes, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::PassPriority)
        .expect("the pooled mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().stack.len(), 1, "the spell was cast once");
    assert!(runner.state().pending_cast.is_none(), "payment finalized");
    counters
}

#[test]
fn autogenerator_counter_replacement_order_decides_the_mana() {
    let mut results = vec![
        autogenerator_counter_order(0),
        autogenerator_counter_order(1),
    ];
    results.sort_unstable();
    assert_eq!(results, vec![3, 4], "add-then-double 4, double-then-add 3");
}

/// CR 601.2g + CR 605.3a + CR 107.1c: Matter Reshaper ({2}{C}) needs a colourless
/// shard. Metalworker produces {C}, so with two other artifact cards in hand and
/// an empty pool the cast is not rejected on the colourless-shard check: Auto
/// hands payment to the payer, who reveals both and completes it.
#[test]
fn auto_cast_matter_reshaper_counts_metalworker_for_the_colorless_shard() {
    use engine::types::mana::ManaCostShard;
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
        .add_spell_to_hand(P0, "Matter Reshaper", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the colorless shard must not reject a cast Metalworker can pay");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "Auto hands the payment to the payer, got {:?}",
        runner.state().waiting_for
    );

    activate(&mut runner, metalworker);
    runner
        .act(GameAction::SelectCards {
            cards: vec![art1, art2],
        })
        .expect("reveal selection is legal");
    assert_eq!(colorless(&runner), 4);
    runner
        .act(GameAction::PassPriority)
        .expect("the pooled mana pays the spell");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none(), "payment finalized");
}

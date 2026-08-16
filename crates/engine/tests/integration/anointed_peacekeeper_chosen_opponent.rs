//! CR 608.2d + CR 608.2c + CR 701.20e — Anointed Peacekeeper / Sorcerous Spyglass:
//! "As ~ enters, look at an opponent's hand, then choose any card name."
//!
//! In a 3+ player game the controller MUST choose WHICH opponent to look at
//! (CR 608.2d), and the private look — plus the subsequent card-name choice —
//! binds against THAT chosen opponent's hand (CR 608.2c "that player"). Regression
//! guard for PR #5042: the prior lowering produced
//! `RevealHand { target: Typed(Opponent), reveal: false }`, which fans out across
//! every opponent, and `reveal_hand::resolve` took `.first()` — silently forcing
//! the FIRST opponent and giving the controller no choice.
//!
//! The fix fronts an explicit `Choose(Opponent)` in the as-enters composition
//! (`parse_as_enters_choose` → `front_opponent_choice_for_nontargeted_look`) and
//! rebinds the look to `ControllerRef::ChosenPlayer { index: 0 }`.
//!
//! These tests drive the REAL apply() pipeline (cast → resolve the Moved
//! replacement → answer the fronted Choose(Opponent) → private look binds to the
//! CHOSEN opponent → answer the card-name choice), NOT a hand-built state.

use engine::game::scenario::GameScenario;
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (data/card-data.json) — building the test card from the
// real card's exact text ensures the parser takes the production branch.
const PEACEKEEPER: &str = "Vigilance\n\
     As this creature enters, look at an opponent's hand, then choose any card name.\n\
     Spells your opponents cast with the chosen name cost {2} more to cast.\n\
     Activated abilities of sources with the chosen name cost {2} more to activate \
     unless they're mana abilities.";

// A card name guaranteed to be in `all_card_names` so the CardName choice is
// accepted by the `ChooseOption` handler (which validates against that set).
const NAMED_CARD: &str = "Llanowar Elves";

/// Cast the Peacekeeper for P0 and resolve it off the stack; the Moved as-enters
/// replacement must pause on the fronted `Choose(Opponent)`.
fn cast_and_reach_opponent_choice(
    runner: &mut engine::game::scenario::GameRunner,
    peace: engine::types::identifiers::ObjectId,
) {
    let card_id = runner.state().objects.get(&peace).unwrap().card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: peace,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Anointed Peacekeeper");
    runner.advance_until_stack_empty();
}

/// 3-player discriminating regression: the controller genuinely chooses which
/// opponent to look at, and picking the NON-first opponent (P2) binds the look —
/// and the card name — to P2's hand, not P1's.
///
/// Revert-failing assertions: without the fronted `Choose(Opponent)`, casting the
/// Peacekeeper never pauses on a `ChoiceType::Opponent` prompt (the first
/// assertion panics), and the look would silently target P1 (the `.first()`
/// opponent) instead of the chosen P2 (`private_look_ids` assertions flip).
#[test]
fn chosen_opponent_binds_private_look_and_card_name_in_three_player() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (controller) holds a card — proves P0's own hand is never looked at.
    let p0_card = scenario.add_card_to_hand(P0, "Controller Secret");
    // Each opponent holds a DIFFERENT distinguishable card.
    let p1_card = scenario.add_card_to_hand(P1, "P1 Only Card");
    let p2_card = scenario.add_card_to_hand(P2, "P2 Only Card");

    let peace = {
        let mut b = scenario.add_creature_to_hand(P0, "Anointed Peacekeeper", 2, 2);
        b.from_oracle_text(PEACEKEEPER);
        b.id()
    };

    let mut runner = scenario.build();
    // Make the named card acceptable to the CardName choice handler.
    runner.state_mut().all_card_names = std::sync::Arc::from([NAMED_CARD.to_string()]);

    cast_and_reach_opponent_choice(&mut runner, peace);

    // The fronted opponent choice: a genuine choice offering BOTH opponents.
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "as-enters look-at-hand must pause on the fronted opponent choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        matches!(
            choice_type,
            engine::types::ability::ChoiceType::Opponent { .. }
        ),
        "the fronted choice must be Choose(Opponent), got {:?}",
        choice_type
    );
    assert!(
        options.contains(&P1.0.to_string()) && options.contains(&P2.0.to_string()),
        "both opponents must be offered (genuine CR 608.2d choice, not auto-first), got {:?}",
        options
    );

    // Choose the NON-first opponent, P2.
    runner
        .act(GameAction::ChooseOption {
            choice: P2.0.to_string(),
        })
        .expect("choose opponent P2");

    // The private look must bind to P2's hand only — CR 608.2c "that player".
    assert_eq!(
        runner.state().private_look_player,
        Some(P0),
        "the controller P0 must be the one looking (CR 701.20e)"
    );
    assert!(
        runner.state().private_look_ids.contains(&p2_card),
        "the look must reveal the CHOSEN opponent P2's card"
    );
    assert!(
        !runner.state().private_look_ids.contains(&p1_card),
        "the look must NOT reveal the non-chosen opponent P1's card"
    );
    assert!(
        !runner.state().private_look_ids.contains(&p0_card),
        "the look must NOT reveal the controller P0's own card"
    );

    // The chain advances to the card-name choice.
    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "after the look the chain must pause on the card-name choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(
        matches!(choice_type, engine::types::ability::ChoiceType::CardName),
        "the second choice must be Choose(CardName), got {:?}",
        choice_type
    );

    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("choose the card name");

    // CardName persists on the Peacekeeper AND (Correction 2) the stray
    // `ChosenAttribute::Player(P2)` from the persisted opponent choice co-exists
    // without breaking the CardName read — co-existence, not exclusivity.
    let attrs = &runner
        .state()
        .objects
        .get(&peace)
        .unwrap()
        .chosen_attributes;
    assert!(
        attrs
            .iter()
            .any(|a| matches!(a, ChosenAttribute::CardName(name) if name == NAMED_CARD)),
        "the chosen card name must persist on the Peacekeeper, got {:?}",
        attrs
    );
    assert!(
        attrs
            .iter()
            .any(|a| matches!(a, ChosenAttribute::Player(pid) if *pid == P2)),
        "the persisted opponent choice leaves a ChosenAttribute::Player(P2) co-present \
         (Correction 2), got {:?}",
        attrs
    );

    // The entry completes: control returns to a normal Priority window.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the entry must complete back to Priority, got {}",
        runner.waiting_for_kind()
    );
}

/// 2-player: with a single opponent, the fronted choice still resolves (only one
/// legal option), the private look binds to that opponent, and the chain reaches
/// the card-name choice.
#[test]
fn single_opponent_still_binds_and_reaches_card_name() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let p1_card = scenario.add_card_to_hand(P1, "P1 Only Card");
    let peace = {
        let mut b = scenario.add_creature_to_hand(P0, "Anointed Peacekeeper", 2, 2);
        b.from_oracle_text(PEACEKEEPER);
        b.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = std::sync::Arc::from([NAMED_CARD.to_string()]);

    cast_and_reach_opponent_choice(&mut runner, peace);

    // Even with one opponent the fronted choice is surfaced (CR 608.2d).
    let WaitingFor::NamedChoice {
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "must pause on the fronted opponent choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(
        choice_type,
        engine::types::ability::ChoiceType::Opponent { .. }
    ));
    assert_eq!(
        options,
        vec![P1.0.to_string()],
        "only P1 is a legal opponent"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose the sole opponent");

    assert_eq!(runner.state().private_look_player, Some(P0));
    assert!(runner.state().private_look_ids.contains(&p1_card));

    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "must reach the card-name choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(
        choice_type,
        engine::types::ability::ChoiceType::CardName
    ));

    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("choose the card name");

    let attrs = &runner
        .state()
        .objects
        .get(&peace)
        .unwrap()
        .chosen_attributes;
    assert!(attrs
        .iter()
        .any(|a| matches!(a, ChosenAttribute::CardName(name) if name == NAMED_CARD)));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Empty-hand chosen opponent: looking at an opponent with no cards is a no-op
/// look (no `MissingParam` error), and the chain STILL reaches the card-name
/// choice — the fronted rebinding must resolve to a real (empty) hand, not fail
/// closed.
#[test]
fn empty_hand_chosen_opponent_is_noop_and_reaches_card_name() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P1 holds a card; P2 (the one we choose) holds NOTHING.
    let _p1_card = scenario.add_card_to_hand(P1, "P1 Only Card");
    let peace = {
        let mut b = scenario.add_creature_to_hand(P0, "Anointed Peacekeeper", 2, 2);
        b.from_oracle_text(PEACEKEEPER);
        b.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = std::sync::Arc::from([NAMED_CARD.to_string()]);

    cast_and_reach_opponent_choice(&mut runner, peace);

    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "must pause on the fronted opponent choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(
        choice_type,
        engine::types::ability::ChoiceType::Opponent { .. }
    ));

    // Choose the empty-handed opponent P2.
    runner
        .act(GameAction::ChooseOption {
            choice: P2.0.to_string(),
        })
        .expect("choose the empty-handed opponent P2");

    // No-op look: no cards revealed, no MissingParam surfaced (the act above
    // succeeded), and the chain still advances to the card-name choice.
    assert!(
        runner.state().private_look_ids.is_empty(),
        "looking at an empty hand reveals nothing"
    );
    let WaitingFor::NamedChoice { choice_type, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "empty-hand look must still reach the card-name choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(matches!(
        choice_type,
        engine::types::ability::ChoiceType::CardName
    ));

    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("choose the card name");

    let attrs = &runner
        .state()
        .objects
        .get(&peace)
        .unwrap()
        .chosen_attributes;
    assert!(attrs
        .iter()
        .any(|a| matches!(a, ChosenAttribute::CardName(name) if name == NAMED_CARD)));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

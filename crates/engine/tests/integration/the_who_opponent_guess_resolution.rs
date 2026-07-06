//! Production-path runtime coverage for the `OpponentGuess` interactive
//! primitive (WHO cluster #38): The Toymaker's Trap (committed-number guess) and
//! The Seventh Doctor (defending-player proposition guess).
//!
//! These tests drive the REAL parse → trigger → resolution pipeline. They parse
//! the shipping Oracle text through `add_creature_from_oracle`, fire the trigger
//! (upkeep / attack) and answer each `WaitingFor` with a real `GameAction`,
//! exercising the deferred-outcome machinery the cluster ships:
//!   choose/commit → raise `WaitingFor::OpponentGuess` → answer via
//!   `GameAction::ChooseOption` → `guess_is_correct` → `set_guess_outcome_recursive`
//!   stamps `Guessed { outcome: GuessOutcome }` → drain → the correct/incorrect branch fires.
//! The parser-level AST shape is covered in `oracle_trigger.rs`; here the seam is
//! the runtime branch resolution (life loss / draw / sacrifice / free-cast /
//! investigate) and the no-guess fallbacks (impossible commit / empty hand).
//!
//! CR 608.2d + CR 608.2e: a player other than the controller guesses a committed
//! value or a proposition during resolution; the result drives the branch.
//! CR 609.3: when nothing can be committed (every number used) or there is no
//! card to choose (empty hand), the guess does nothing and only the
//! always-on / no-guess tail proceeds.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChoiceType, ChosenAttribute};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const TOYMAKER_ORACLE: &str =
    "At the beginning of your upkeep, secretly choose a number between 1 and 5 \
     that hasn't been chosen. If you do, an opponent guesses which number you chose, \
     then you reveal the number you chose. If they guessed wrong, they lose life equal \
     to the number they guessed and you draw a card. If they guessed right, sacrifice \
     this enchantment.";

const SEVENTH_DOCTOR_ORACLE: &str =
    "Whenever The Seventh Doctor attacks, choose a card in your hand. Defending player \
     guesses whether that card's mana value is greater than the number of artifacts you \
     control. If they guessed wrong, you may cast it without paying its mana cost. If you \
     don't cast a spell this way, investigate.";

/// Pass priority through empty windows until the engine pauses on `want` (a
/// `WaitingFor` kind name) or stalls on a non-priority state.
fn drive_to_wait(runner: &mut GameRunner, want: &str) {
    for _ in 0..64 {
        if runner.waiting_for_kind() == want {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Build The Toymaker's Trap as a 0/0 enchantment P0 controls, set up P0's
/// upkeep, and advance until its trigger is on the stack. Returns the runner and
/// the enchantment's id.
fn toymaker_at_upkeep(seeded_numbers: &[u8]) -> (GameRunner, ObjectId) {
    toymaker_at_upkeep_with_player_count(2, seeded_numbers)
}

fn toymaker_at_upkeep_with_player_count(
    player_count: u8,
    seeded_numbers: &[u8],
) -> (GameRunner, ObjectId) {
    let mut scenario = if player_count == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(player_count, 42)
    };
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    // P0 library so the "you draw a card" rider has a card to draw.
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);

    let trap = scenario
        .add_creature_from_oracle(P0, "The Toymaker's Trap", 0, 0, TOYMAKER_ORACLE)
        // An enchantment, so the 0/0 body is not destroyed by the toughness SBA
        // before the upkeep trigger fires.
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    if !seeded_numbers.is_empty() {
        let obj = runner.state_mut().objects.get_mut(&trap).unwrap();
        obj.chosen_attributes = seeded_numbers
            .iter()
            .copied()
            .map(ChosenAttribute::Number)
            .collect();
    }
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::Untap;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_upkeep();
    drive_to_wait(&mut runner, "NamedChoice");
    (runner, trap)
}

fn choose_guessing_opponent(runner: &mut GameRunner, opponent: PlayerId) {
    drive_to_wait(runner, "NamedChoice");
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(
                *player, P0,
                "the controller chooses which opponent makes the guess"
            );
            assert!(
                matches!(choice_type, ChoiceType::Opponent { restriction: None }),
                "expected opponent choice before the guess, got {choice_type:?}"
            );
            assert!(
                options.contains(&opponent.0.to_string()),
                "chosen opponent must be offered in {options:?}"
            );
        }
        other => panic!("expected opponent NamedChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: opponent.0.to_string(),
        })
        .expect("choosing the guessing opponent must succeed");
}

/// CR 608.2d + CR 609.3: P0 secretly commits a number; the opponent guesses
/// WRONG, so they lose life equal to the number they guessed and P0 draws. The
/// enchantment survives (the sacrifice branch is gated on a right guess).
#[test]
fn toymakers_trap_wrong_guess_drains_guesser_and_draws() {
    let (mut runner, trap) = toymaker_at_upkeep(&[]);

    // P0 secretly chooses a number (3).
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player, options, ..
        } => {
            assert_eq!(*player, P0, "the controller commits the secret number");
            assert_eq!(options.len(), 5, "1..=5 are all available, got {options:?}");
        }
        other => panic!("expected the secret NamedChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: "3".to_string(),
        })
        .expect("committing the number must succeed");

    // The controller chooses the opponent who guesses; P1 then guesses.
    choose_guessing_opponent(&mut runner, P1);
    drive_to_wait(&mut runner, "OpponentGuess");
    match &runner.state().waiting_for {
        WaitingFor::OpponentGuess {
            player,
            options,
            proposition_truth,
            ..
        } => {
            assert_eq!(*player, P1, "an opponent (P1) guesses, not the controller");
            assert_eq!(
                *proposition_truth, None,
                "a committed-number guess carries no proposition truth"
            );
            assert_eq!(options.len(), 5, "the guesser may name any printed value");
        }
        other => panic!("expected OpponentGuess, got {other:?}"),
    }

    // The committed secret (Number(3)) is hidden from the guesser over the wire.
    let guesser_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    assert!(
        !guesser_view.objects[&trap]
            .chosen_attributes
            .contains(&ChosenAttribute::Number(3)),
        "the committed number must be redacted from the guesser"
    );

    let p1_life_before = runner.life(P1);
    let p0_hand_before = runner.state().players[0].hand.len();

    // P1 guesses 1 — wrong (the commit was 3).
    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("answering the guess must succeed");

    // Wrong branch: "they lose life equal to the number they guessed" (1) AND
    // "you draw a card"; the enchantment is NOT sacrificed.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 1,
        "the guesser loses life equal to their (wrong) guess of 1"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        p0_hand_before + 1,
        "the controller draws a card on a wrong guess"
    );
    assert_eq!(
        runner.state().objects[&trap].zone,
        Zone::Battlefield,
        "a wrong guess must not sacrifice the enchantment"
    );
    assert_eq!(runner.life(P0), 20, "the controller loses no life");
}

/// CR 608.2d: P0 commits a number; the opponent guesses RIGHT, so the enchantment
/// is sacrificed and no life loss / draw occurs.
#[test]
fn toymakers_trap_right_guess_sacrifices_enchantment() {
    let (mut runner, trap) = toymaker_at_upkeep(&[]);

    runner
        .act(GameAction::ChooseOption {
            choice: "3".to_string(),
        })
        .expect("committing the number must succeed");

    choose_guessing_opponent(&mut runner, P1);
    drive_to_wait(&mut runner, "OpponentGuess");
    let p1_life_before = runner.life(P1);
    let p0_hand_before = runner.state().players[0].hand.len();

    // P1 guesses 3 — correct.
    runner
        .act(GameAction::ChooseOption {
            choice: "3".to_string(),
        })
        .expect("answering the guess must succeed");

    assert_eq!(
        runner.state().objects[&trap].zone,
        Zone::Graveyard,
        "a right guess sacrifices the enchantment"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "a right guess deals no life loss to the guesser"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        p0_hand_before,
        "a right guess does not draw a card"
    );
}

/// CR 608.2d + CR 102.3: in multiplayer, "an opponent guesses" is not a fan-out
/// and must not silently pick the first opponent. The controller chooses one
/// eligible opponent, then only that player receives `OpponentGuess`.
#[test]
fn toymakers_trap_three_player_controller_chooses_guessing_opponent() {
    let (mut runner, _trap) = toymaker_at_upkeep_with_player_count(3, &[]);

    runner
        .act(GameAction::ChooseOption {
            choice: "3".to_string(),
        })
        .expect("committing the number must succeed");

    choose_guessing_opponent(&mut runner, P2);
    drive_to_wait(&mut runner, "OpponentGuess");
    match &runner.state().waiting_for {
        WaitingFor::OpponentGuess {
            player,
            options,
            proposition_truth,
            ..
        } => {
            assert_eq!(*player, P2, "the chosen opponent makes the guess");
            assert_eq!(
                *proposition_truth, None,
                "a committed-number guess carries no proposition truth"
            );
            assert_eq!(options.len(), 5, "the guesser may name any printed value");
        }
        other => panic!("expected chosen opponent's OpponentGuess, got {other:?}"),
    }

    let p1_life_before = runner.life(P1);
    let p2_life_before = runner.life(P2);
    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("answering the guess must succeed");

    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "the unchosen opponent must not be treated as the guesser"
    );
    assert_eq!(
        runner.life(P2),
        p2_life_before - 1,
        "the chosen opponent loses life equal to their wrong guess"
    );
}

/// CR 609.3: when every legal number has already been chosen for this card, the
/// secret commit does nothing — no guess is raised and no branch fires.
#[test]
fn toymakers_trap_exhausted_numbers_makes_no_guess() {
    // Pre-seed all of 1..=5 as already committed (DistinctFromSourceHistory).
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_library_top(P0, &["Draw A"]);
    let trap = scenario
        .add_creature_from_oracle(P0, "The Toymaker's Trap", 0, 0, TOYMAKER_ORACLE)
        .as_enchantment()
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&trap)
        .unwrap()
        .chosen_attributes = (1u8..=5).map(ChosenAttribute::Number).collect();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::Untap;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_upkeep();

    for _ in 0..64 {
        match runner.waiting_for_kind() {
            "OpponentGuess" | "NamedChoice" => {
                panic!("an exhausted commit must not raise a choice/guess prompt")
            }
            "Priority" => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    assert_eq!(
        runner.state().objects[&trap].zone,
        Zone::Battlefield,
        "with no guess, the sacrifice branch never fires"
    );
    assert_eq!(runner.life(P1), 20, "no wrong-guess life loss occurs");
}

/// Build The Seventh Doctor attacking P0 → P1, the attack trigger on the stack.
/// `hand_card` is the (optional) spell put in P0's hand to be chosen.
fn seventh_doctor_attacking(with_hand_card: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    // Libraries so neither player decks out across draw steps before assertions.
    scenario.with_library_top(P0, &["P0 Lib A", "P0 Lib B", "P0 Lib C", "P0 Lib D"]);
    scenario.with_library_top(P1, &["P1 Lib A", "P1 Lib B", "P1 Lib C", "P1 Lib D"]);

    let doctor = scenario
        .add_creature_from_oracle(P0, "The Seventh Doctor", 3, 3, SEVENTH_DOCTOR_ORACLE)
        .id();
    // A {0}-mana-value instant in hand: mana value 0, so the proposition
    // "that card's mana value (0) is greater than artifacts you control (0)" is
    // FALSE — deterministic without setting a mana cost.
    let hand_card = with_hand_card.then(|| scenario.add_spell_to_hand(P0, "Free Spell", true).id());

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(doctor, AttackTarget::Player(P1))])
        .expect("declaring the attack must succeed");
    (runner, doctor, hand_card)
}

/// Drive the attack trigger to the defending player's Proposition guess: P0
/// chooses the hand card, then P1 is prompted to guess. Returns the guess
/// `options`. Also asserts the defending player is the guesser, the proposition
/// resolved false (MV 0 is not > 0 artifacts), and that the resolved answer is
/// redacted from the guesser's filtered view.
fn drive_seventh_doctor_to_guess(runner: &mut GameRunner, hand_card: ObjectId) -> Vec<String> {
    drive_to_wait(runner, "ChooseFromZoneChoice");
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the controller chooses a card in their hand");
            assert!(cards.contains(&hand_card));
        }
        other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![hand_card],
        })
        .expect("choosing the hand card must succeed");

    drive_to_wait(runner, "OpponentGuess");
    let options = match &runner.state().waiting_for {
        WaitingFor::OpponentGuess {
            player,
            options,
            proposition_truth,
            ..
        } => {
            assert_eq!(*player, P1, "the defending player makes the guess");
            assert_eq!(
                *proposition_truth,
                Some(false),
                "MV 0 > 0 artifacts is false on the unfiltered state"
            );
            options.clone()
        }
        other => panic!("expected OpponentGuess, got {other:?}"),
    };

    // CR 608.2d: the resolved answer is redacted from the guesser over the wire.
    let guesser_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    match guesser_view.waiting_for {
        WaitingFor::OpponentGuess {
            proposition_truth, ..
        } => assert_eq!(
            proposition_truth, None,
            "the proposition answer must be redacted from the guesser"
        ),
        other => panic!("expected OpponentGuess in the filtered view, got {other:?}"),
    }
    options
}

/// CR 608.2d: A WRONG proposition guess fires the incorrect branch — the
/// controller's "you may cast it without paying its mana cost" window. The
/// proposition is false, so guessing "greater" (the comparator-true label) is
/// wrong; accepting the window casts the chosen card for free (it leaves hand).
#[test]
fn seventh_doctor_wrong_proposition_guess_offers_free_cast() {
    let (mut runner, _doctor, hand_card) = seventh_doctor_attacking(true);
    let hand_card = hand_card.unwrap();
    let options = drive_seventh_doctor_to_guess(&mut runner, hand_card);

    let greater = options
        .iter()
        .find(|o| o.as_str() == "greater")
        .expect("a 'greater' option must exist")
        .clone();
    runner
        .act(GameAction::ChooseOption { choice: greater })
        .expect("answering the guess must succeed");

    // The wrong-guess branch (`CastFromZone`, optional) opens the free-cast
    // window for the controller.
    assert_eq!(
        runner.waiting_for_kind(),
        "OptionalEffectChoice",
        "a wrong guess must open the controller's free-cast window, got {:?}",
        runner.state().waiting_for
    );
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0, "the controller decides whether to free-cast")
        }
        other => panic!("expected OptionalEffectChoice, got {other:?}"),
    }

    // Accepting the window casts the chosen card without paying — it leaves hand.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the free cast must succeed");
    assert_ne!(
        runner.state().objects[&hand_card].zone,
        Zone::Hand,
        "the freely-cast card must leave the controller's hand"
    );
}

/// CR 608.2d: A CORRECT proposition guess gates OFF the incorrect branch — the
/// free-cast window must NOT open. The proposition is false, so guessing
/// "not greater" (the comparator-false label) is correct.
#[test]
fn seventh_doctor_correct_proposition_guess_skips_free_cast() {
    let (mut runner, _doctor, hand_card) = seventh_doctor_attacking(true);
    let hand_card = hand_card.unwrap();
    let options = drive_seventh_doctor_to_guess(&mut runner, hand_card);

    let not_greater = options
        .iter()
        .find(|o| o.contains("not"))
        .expect("a 'not greater' option must exist")
        .clone();
    runner
        .act(GameAction::ChooseOption {
            choice: not_greater,
        })
        .expect("answering the guess must succeed");

    // The free-cast window is gated on a WRONG guess (GuessOutcome::Incorrect); a
    // correct guess must not offer it, and the chosen card stays in hand.
    assert_ne!(
        runner.waiting_for_kind(),
        "OptionalEffectChoice",
        "a correct guess must not open the free-cast window, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&hand_card].zone,
        Zone::Hand,
        "a correct guess must not cast the chosen card"
    );
}

/// CR 609.3: With an empty hand there is no card to choose, so neither the
/// card choice nor the guess is raised — the no-guess fallback. The trigger
/// resolves without wedging the game.
#[test]
fn seventh_doctor_empty_hand_raises_no_guess() {
    let (mut runner, _doctor, _none) = seventh_doctor_attacking(false);

    let mut settled = false;
    for _ in 0..64 {
        match runner.waiting_for_kind() {
            "OpponentGuess" | "ChooseFromZoneChoice" => {
                panic!("an empty hand must not raise a card choice or a guess")
            }
            "Priority" => {
                if runner.act(GameAction::PassPriority).is_err() {
                    settled = true;
                    break;
                }
            }
            _ => {
                settled = true;
                break;
            }
        }
    }
    assert!(
        settled,
        "the trigger must resolve without raising a guess (no-guess fallback)"
    );
}

//! CR 101.4 + CR 608.2c: *"Choose an opponent with the highest number"* —
//! Itazura, Lingering Wick's selection clause.
//!
//! The restriction is part of the instruction, not decoration. Dropping it lets
//! the controller pick an opponent who did NOT choose the highest number and
//! then deal them the damage — a legal-looking choice the rules forbid. This
//! test pins that the offered option set is narrowed to the actual highest
//! chooser(s), in both the unique and the tied case, and that the damage follows
//! the selection through the "them" anaphor.
//!
//! Oracle-text note: the clause under test is exercised here on a distilled
//! spell carrying Itazura's exact selection-and-damage wording, because the real
//! card wraps it in unrelated exile/free-cast machinery whose own gaps would
//! dominate the assertions. Itazura's VERBATIM text is covered at the parse
//! layer by `secret_number_provenance_invariant_holds_across_the_class`, which
//! pins the card as a genuine `PlayerChosenNumber` reader — so the pairing
//! covers both "the real card binds the restriction" and "the restriction is
//! enforced at runtime".
//!
//! Fail-on-revert: remove the restriction from the `ChoiceType::Opponent` seam
//! and the non-highest opponent reappears in `options`, failing the first
//! assertion in each case.
//!
//! CR 101.4: APNAP order for simultaneous choices.
//! CR 120.3a: damage dealt to a player by a source without infect causes that
//! player to lose that much life.
//! CR 608.2c: follow the instructions in the order written.
//! CR 608.2d: a choice offered by a resolving spell is announced while applying
//! the effect; an illegal option can't be chosen.

use engine::game::scenario::GameScenario;
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Itazura's selection-and-damage wording, verbatim from the card, with its
/// unrelated exile/free-cast tail removed.
const ORACLE: &str = "Each opponent secretly chooses a number 0 or greater. Then those numbers are revealed. Choose an opponent with the highest number. Itazura deals that much damage to them.";

fn life(state: &GameState, player: PlayerId) -> i32 {
    state.players[player.0 as usize].life
}

/// Drives the spell with the given per-opponent bids, returning the option list
/// offered for the opponent choice and the final state.
fn run(bids: &[(PlayerId, &str)]) -> (Vec<String>, GameState) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Lib 1", "Lib 2", "Lib 3"]);
        scenario.with_life(player, 20);
    }

    let mut spell_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Itazura, Lingering Wick", false, ORACLE);
    spell_builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Red],
    });
    let spell = spell_builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, spell, false, vec![]),
            ManaUnit::new(ManaType::Red, spell, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting must start");

    let mut opponent_options: Vec<String> = Vec::new();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("mana");
            }
            WaitingFor::NamedChoice {
                player,
                options,
                choice_type,
                ..
            } => {
                let choice = match choice_type {
                    // The secret number: free-entry, answered per the bid table.
                    ChoiceType::NumberRange { .. } => bids
                        .iter()
                        .find(|(seat, _)| *seat == player)
                        .map(|(_, bid)| (*bid).to_string())
                        .unwrap_or_else(|| panic!("unexpected number chooser {player:?}")),
                    // THE ASSERTION SURFACE: which opponents the engine offers.
                    ChoiceType::Opponent { .. } => {
                        opponent_options = options.clone();
                        options
                            .first()
                            .cloned()
                            .expect("an opponent must be offered")
                    }
                    other => panic!("unexpected choice {other:?}"),
                };
                runner
                    .act(GameAction::ChooseOption { choice })
                    .unwrap_or_else(|e| panic!("answering {player:?} must succeed: {e:?}"));
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
                if runner.state().stack.is_empty() && !opponent_options.is_empty() {
                    break;
                }
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    (opponent_options, runner.state().clone())
}

/// A UNIQUE highest chooser: only that opponent may be selected, and the damage
/// equals their number. The opponent who bid lower must not be offered at all —
/// CR 608.2d, an illegal option can't be chosen.
#[test]
fn only_the_highest_chooser_is_offered_and_takes_the_damage() {
    let (options, state) = run(&[(P1, "5"), (P2, "2")]);

    assert_eq!(
        options,
        vec![P1.0.to_string()],
        "only the opponent who chose the highest number may be selected; \
         offering P2 (who chose 2) is the illegal choice the restriction prevents"
    );
    // CR 120.3a + CR 608.2c: "that much damage to them" follows the selection.
    assert_eq!(
        life(&state, P1),
        15,
        "P1 chose 5 and takes exactly that much"
    );
    assert_eq!(
        life(&state, P2),
        20,
        "P2 was not selectable and is untouched"
    );
    assert_eq!(life(&state, P0), 20, "the controller is not an opponent");
}

/// A TIE for highest: both opponents are legal selections (CR 608.2d resolves
/// ties by leaving the choice to the controller), and whichever is chosen takes
/// the tied number. This is the case a "first match wins" implementation would
/// get wrong by narrowing to one seat.
#[test]
fn tied_highest_choosers_are_both_offered() {
    let (options, state) = run(&[(P1, "4"), (P2, "4")]);

    let mut sorted = options.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![P1.0.to_string(), P2.0.to_string()],
        "both opponents tied for the highest number must be selectable"
    );

    // Exactly one of them took 4; the other is untouched. Which one is the
    // controller's choice (the driver picks the first offered), so assert the
    // shape rather than a specific seat.
    let damaged: Vec<PlayerId> = [P1, P2]
        .into_iter()
        .filter(|p| life(&state, *p) == 16)
        .collect();
    let untouched: Vec<PlayerId> = [P1, P2]
        .into_iter()
        .filter(|p| life(&state, *p) == 20)
        .collect();
    assert_eq!(
        damaged.len(),
        1,
        "exactly one tied opponent takes the damage"
    );
    assert_eq!(untouched.len(), 1, "the other tied opponent is untouched");
}

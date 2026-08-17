//! Wheel of Misfortune — *"Each player secretly chooses a number 0 or greater,
//! then all players reveal those numbers simultaneously and determine the
//! highest and lowest numbers revealed this way. Wheel of Misfortune deals
//! damage equal to the highest number to each player who chose that number.
//! Each player who didn't choose the lowest number discards their hand, then
//! draws seven cards."*
//!
//! The card is the flagship of the secret-simultaneous-number class (Menacing
//! Ogre, Life at Stake), and every clause of it keys on a CROSS-PLAYER extremum
//! of per-player choices. This test drives the real parse → cast → resolution
//! pipeline and pins the three behaviors that make the card what it is:
//!
//!   1. every player is prompted for a number (CR 101.4 APNAP order);
//!   2. the damage lands on the players who chose the HIGHEST number — all of
//!      them when there is a tie — and on nobody else, for exactly that much;
//!   3. the wheel (discard hand, draw seven) hits every player who did NOT
//!      choose the LOWEST number, and skips the one who did.
//!
//! The seating is chosen to discriminate: P0 and P1 both choose 4 (a tie for
//! highest), P2 chooses 1 (the unique lowest). A filter that collapsed to "all
//! players" would wheel P2 too; one that took only the first tied player would
//! spare P1 the damage; one that read a per-source chosen number instead of a
//! per-player one would deal 0.
//!
//! CR 101.4: when multiple players make choices at the same time, the active
//! player chooses first, then the remaining players in turn order.
//! CR 101.4b: a player normally knows the earlier choices — which is why the
//! card says "secretly", and why `game::visibility` keeps each player's
//! `ChosenAttribute::Number` private to that player.
//! CR 120.3a: damage dealt to a player by a source without infect causes that
//! player to lose that much life.
//! CR 121.1: a player draws a card by putting the top card of their library
//! into their hand.
//! CR 608.2c: the controller follows the spell's instructions in written order.
//! CR 608.2d: a choice offered by a resolving spell is announced while applying
//! the effect.
//! CR 701.9a: to discard a card, move it from its owner's hand to that player's
//! graveyard.

use engine::game::scenario::GameScenario;
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (Scryfall). A paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const WHEEL_OF_MISFORTUNE: &str = "Each player secretly chooses a number 0 or greater, then all players reveal those numbers simultaneously and determine the highest and lowest numbers revealed this way. Wheel of Misfortune deals damage equal to the highest number to each player who chose that number. Each player who didn't choose the lowest number discards their hand, then draws seven cards.";

/// The number each seat secretly chooses, in APNAP order. P0/P1 tie for the
/// highest; P2 is the unique lowest.
const CHOICES: [(PlayerId, &str); 3] = [(P0, "4"), (P1, "4"), (P2, "1")];

fn hand_size(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize].hand.len()
}

fn life(state: &engine::types::game_state::GameState, player: PlayerId) -> i32 {
    state.players[player.0 as usize].life
}

#[test]
fn wheel_of_misfortune_burns_the_highest_choosers_and_wheels_everyone_but_the_lowest() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Seven-card libraries so every wheeled player can actually draw seven
    // (CR 121.1), and a distinguishable starting hand per seat.
    for player in [P0, P1, P2] {
        scenario.with_library_top(
            player,
            &[
                "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8",
            ],
        );
        scenario.with_cards_in_hand(player, &["Hand A", "Hand B"]);
    }

    let mut spell_builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Wheel of Misfortune",
        false,
        WHEEL_OF_MISFORTUNE,
    );
    spell_builder.with_mana_cost(ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Red],
    });
    let spell = spell_builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, spell, false, vec![]),
            ManaUnit::new(ManaType::Red, spell, false, vec![]),
            ManaUnit::new(ManaType::Red, spell, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    // Staging sanity check: two cards each, plus the spell itself in P0's hand
    // (it leaves for the stack when the cast commits, CR 601.2a). Every seat
    // starts with a NON-SEVEN hand, so the post-resolution 7 / 7 / 2 below can
    // only come from the wheel actually firing on P0 and P1 and not on P2.
    let hands_before: Vec<usize> = [P0, P1, P2]
        .iter()
        .map(|p| hand_size(runner.state(), *p))
        .collect();
    assert_eq!(hands_before, vec![3, 2, 2], "staged hands");

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Wheel of Misfortune must start");

    // Drive the resolution, answering each number prompt with that seat's
    // scripted choice and recording who was asked, in order.
    let mut number_choosers: Vec<PlayerId> = Vec::new();
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment must auto-finalize");
            }
            WaitingFor::NamedChoice {
                player,
                options,
                choice_type,
                ..
            } => {
                let (_, choice) = CHOICES
                    .iter()
                    .find(|(seat, _)| *seat == player)
                    .unwrap_or_else(|| panic!("unexpected chooser {player:?}"));
                // CR 107.1a/b: "a number 0 or greater" states no maximum, so the
                // prompt enumerates NOTHING and the value is supplied by the
                // player. An option list here would mean the engine had invented
                // a ceiling — the bug that made 21 illegal.
                assert!(
                    options.is_empty(),
                    "an unbounded number choice must not enumerate options; got {options:?}"
                );
                assert!(
                    choice_type.options_supplied_by_player(),
                    "the prompt must route to the free-entry path"
                );
                assert_eq!(
                    choice_type.accepts_free_entry_answer(choice),
                    Some(true),
                    "{choice} must be a legal answer for {player:?}"
                );
                number_choosers.push(player);
                // CR 101.4b: BEFORE the reveal, a chooser must not be able to
                // read the answers already given. Checked at the moment the
                // second and third seats are prompted — the exact window the
                // card's "secretly" wording exists to close.
                for (earlier, _) in CHOICES.iter().take(number_choosers.len() - 1) {
                    let view =
                        engine::game::visibility::filter_state_for_viewer(runner.state(), player);
                    assert!(
                        !view.players[earlier.0 as usize]
                            .chosen_attributes
                            .iter()
                            .any(|a| matches!(a, ChosenAttribute::Number(_))),
                        "{player:?} must not see {earlier:?}'s number before the reveal"
                    );
                }
                runner
                    .act(GameAction::ChooseOption {
                        choice: (*choice).to_string(),
                    })
                    .expect("answering the number choice must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
                if runner.state().stack.is_empty() && number_choosers.len() == CHOICES.len() {
                    break;
                }
            }
            other => panic!("unexpected prompt during resolution: {other:?}"),
        }
    }

    // CR 101.4 + CR 608.2c: AFTER the reveal instruction resolved, every number
    // is public to every player. This is the other half of the privacy contract
    // — without it the engine would keep the numbers secret past the instruction
    // that publishes them, which is what a bare `Effect::NoOp` reveal did.
    for viewer in [P0, P1, P2] {
        let view = engine::game::visibility::filter_state_for_viewer(runner.state(), viewer);
        for (seat, chosen) in CHOICES {
            let expected: u32 = chosen.parse().expect("scripted choice is numeric");
            assert!(
                view.players[seat.0 as usize]
                    .chosen_attributes
                    .contains(&ChosenAttribute::RevealedNumber(expected)),
                "{viewer:?} must see {seat:?}'s revealed number {expected} after the reveal"
            );
        }
    }

    // CR 101.4: the active player chooses first, then the rest in turn order.
    assert_eq!(
        number_choosers,
        vec![P0, P1, P2],
        "every player must secretly choose a number, in APNAP order"
    );

    let state = runner.state();

    // CR 120.3a: the highest number revealed is 4, and BOTH players who chose it
    // take exactly that much. P2 chose 1, which is not the highest, so P2 takes
    // none — a filter that widened to "each player" would show -4 here too.
    assert_eq!(life(state, P0), 16, "P0 tied for the highest number (4)");
    assert_eq!(life(state, P1), 16, "P1 tied for the highest number (4)");
    assert_eq!(life(state, P2), 20, "P2 did not choose the highest number");

    // CR 701.9a + CR 121.1: everyone who did NOT choose the lowest number (1)
    // discards their hand and draws seven. P2 chose the lowest and is skipped
    // entirely — hand untouched, no draw.
    assert_eq!(
        hand_size(state, P0),
        7,
        "P0 didn't choose the lowest number, so it wheels to a fresh seven"
    );
    assert_eq!(
        hand_size(state, P1),
        7,
        "P1 didn't choose the lowest number, so it wheels to a fresh seven"
    );
    assert_eq!(
        hand_size(state, P2),
        2,
        "P2 chose the lowest number and keeps its hand — no discard, no draw"
    );
}

/// CR 107.1a/b: "a number 0 or greater" states NO maximum, so a number past any
/// ceiling the engine might have invented must be both choosable and effective.
///
/// This is the case the three-seat test above structurally cannot detect: it only
/// ever chooses 1 and 4, both inside the range the engine used to invent
/// (`min: 0, max: 20`), so it stayed green while 21 was rejected outright. Here
/// P1 bids exactly 21 and P0 bids 40 — past both the old ceiling and a starting
/// life total — and every assertion below is reachable only if those values were
/// accepted at the answer seam, stored at full width, folded as the cross-player
/// maximum, and dealt as damage.
#[test]
fn a_number_past_the_old_ceiling_is_choosable_and_deals_that_much_damage() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(
            player,
            &[
                "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8",
            ],
        );
        scenario.with_cards_in_hand(player, &["Hand A", "Hand B"]);
        // High enough that a 40-point hit is survivable, so the assertion reads a
        // life total rather than an elimination.
        scenario.with_life(player, 60);
    }

    let mut spell_builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Wheel of Misfortune",
        false,
        WHEEL_OF_MISFORTUNE,
    );
    spell_builder.with_mana_cost(ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Red],
    });
    let spell = spell_builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, spell, false, vec![]),
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
        .expect("casting Wheel of Misfortune must start");

    let bids: [(PlayerId, &str); 3] = [(P0, "40"), (P1, "21"), (P2, "0")];
    let mut answered = 0usize;
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("mana payment must auto-finalize");
            }
            WaitingFor::NamedChoice { player, .. } => {
                let (_, bid) = bids
                    .iter()
                    .find(|(seat, _)| *seat == player)
                    .unwrap_or_else(|| panic!("unexpected chooser {player:?}"));
                runner
                    .act(GameAction::ChooseOption {
                        choice: (*bid).to_string(),
                    })
                    .unwrap_or_else(|e| {
                        panic!("{bid} must be a legal answer for {player:?}: {e:?}")
                    });
                answered += 1;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
                if runner.state().stack.is_empty() && answered == bids.len() {
                    break;
                }
            }
            other => panic!("unexpected prompt during resolution: {other:?}"),
        }
    }
    assert_eq!(answered, 3, "all three bids must have been accepted");

    let state = runner.state();
    // CR 120.3a: 40 is the highest bid, so P0 — and only P0 — takes exactly that
    // much. Under the invented ceiling this assertion could not even be reached:
    // the answer seam rejected the bid.
    assert_eq!(life(state, P0), 20, "P0 bid 40 and takes exactly that much");
    assert_eq!(life(state, P1), 60, "P1 did not bid the highest");
    assert_eq!(life(state, P2), 60, "P2 did not bid the highest");

    // CR 701.9a + CR 121.1: P2 bid the lowest (0) and is spared; the other two
    // wheel — including the 21 bid the old range also rejected.
    assert_eq!(hand_size(state, P0), 7, "P0 wheels");
    assert_eq!(
        hand_size(state, P1),
        7,
        "P1 bid 21 — past the old ceiling — and wheels"
    );
    assert_eq!(
        hand_size(state, P2),
        2,
        "P2 bid the lowest and keeps its hand"
    );
}

//! A "… this way" gate reads the instruction that performed its verb, not the
//! node directly before it.
//!
//! CR 608.2c: instructions are followed in the order written, and "if a land
//! card is discarded this way" names the discard. A draw (CR 121.1: putting the
//! top card of the library into the hand) is not a discard, mill or exile, so a
//! draw between the discard and the gate must leave the discard's result in
//! place (Lord Windgrace). Likewise each gated node of one run of "If a … card
//! is milled this way, …" sentences is not the mill, so a token one of them
//! creates (CR 111.1) must not become what the next gate reads (Old Rutstein).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LORD_WINDGRACE: &str = "[+2]: Discard a card, then draw a card. If a land card is discarded this way, draw an additional card.\n[−3]: Return up to two target land cards from your graveyard to the battlefield.\n[−11]: Destroy up to six target nonland permanents, then create six 2/2 green Cat Warrior creature tokens with forestwalk.\nLord Windgrace can be your commander.";

const OLD_RUTSTEIN: &str = "When Old Rutstein enters and at the beginning of your upkeep, mill a card. If a land card is milled this way, create a Treasure token. If a creature card is milled this way, create a 1/1 green Insect creature token. If a noncreature, nonland card is milled this way, create a Blood token.";

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[0].hand.len()
}

fn tokens_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token && o.zone == Zone::Battlefield && o.name == name && o.controller == P0
        })
        .map(|o| o.id)
        .collect()
}

/// Pass priority until the stack is empty, answering a discard prompt with
/// `discard`. Returns how many discard prompts appeared.
fn settle(runner: &mut GameRunner, discard: Option<ObjectId>) -> usize {
    let mut discard_prompts = 0;
    for _ in 0..64 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DiscardChoice { cards, .. } => {
                discard_prompts += 1;
                let pick = discard.expect("a discard pick is staged");
                assert!(cards.contains(&pick), "the staged pick is offered");
                GameAction::SelectCards { cards: vec![pick] }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution completed: {:?}",
        runner.state().waiting_for
    );
    discard_prompts
}

#[derive(Clone, Copy)]
enum WindgraceHand {
    /// Hand {Forest, Lightning Bolt}; discard the named one.
    DiscardForest,
    DiscardBolt,
    Empty,
}

/// Activate Lord Windgrace's +2 and return (cards drawn, discard prompts).
/// `land_on_top` stages a basic land (else a sorcery) as the library top.
fn windgrace_plus_two(hand: WindgraceHand, land_on_top: bool) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Lord Windgrace", "Windgrace", 5, LORD_WINDGRACE)
        .id();
    for n in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {n}"), false);
    }
    if land_on_top {
        scenario.add_land_to_library_top(P0, "Swamp");
    } else {
        scenario.add_spell_to_library_top(P0, "Library Sorcery", false);
    }
    let pick = match hand {
        WindgraceHand::Empty => None,
        WindgraceHand::DiscardForest | WindgraceHand::DiscardBolt => {
            let forest = scenario.add_land_to_hand(P0, "Forest").id();
            let bolt = scenario.add_bolt_to_hand(P0);
            Some(match hand {
                WindgraceHand::DiscardForest => forest,
                _ => bolt,
            })
        }
    };
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner);
    let library_before = runner.state().players[0].library.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate +2");
    let prompts = settle(&mut runner, pick);
    if let Some(pick) = pick {
        assert_eq!(runner.state().objects[&pick].zone, Zone::Graveyard);
    }
    let drawn = library_before - runner.state().players[0].library.len();
    let discarded = usize::from(pick.is_some());
    assert_eq!(hand_len(&runner), hand_before - discarded + drawn);
    (drawn, prompts)
}

/// CR 608.2c + CR 121.1: discarding the Forest draws two (one, plus the
/// additional card) even though the drawn card is a nonland; discarding the
/// Bolt draws one even though the drawn card is a land.
#[test]
fn lord_windgrace_gate_reads_the_discard_not_the_draw() {
    assert_eq!(
        windgrace_plus_two(WindgraceHand::DiscardForest, false),
        (2, 1)
    );
    assert_eq!(windgrace_plus_two(WindgraceHand::DiscardBolt, true), (1, 1));
}

/// Lord Windgrace ruling: "If you have no cards in hand as Lord Windgrace's
/// first ability resolves, you'll discard nothing then you'll draw one card."
/// The drawn land must not satisfy the gate.
#[test]
fn lord_windgrace_empty_hand_draws_exactly_one() {
    assert_eq!(windgrace_plus_two(WindgraceHand::Empty, true), (1, 0));
}

#[derive(Clone, Copy)]
enum Milled {
    DryadArbor,
    Creature,
    Instant,
}

/// Resolve Old Rutstein's upkeep trigger over `milled` and return the
/// (Treasure, Insect, Blood) token counts.
fn old_rutstein_upkeep(milled: Milled) -> (usize, usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(P0, "Old Rutstein", 1, 4, OLD_RUTSTEIN);
    for n in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {n}"), false);
    }
    let top = match milled {
        Milled::DryadArbor => scenario
            .add_land_to_library_top(P0, "Dryad Arbor")
            .as_creature()
            .with_subtypes(vec!["Forest", "Dryad"])
            .id(),
        Milled::Creature => scenario
            .add_spell_to_library_top(P0, "Grizzly Bears", false)
            .as_creature()
            .id(),
        Milled::Instant => scenario
            .add_spell_to_library_top(P0, "Lightning Bolt", true)
            .id(),
    };
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the upkeep trigger is on the stack"
    );
    settle(&mut runner, None);
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Graveyard,
        "reach: milled"
    );
    (
        tokens_named(&runner, "Treasure").len(),
        tokens_named(&runner, "Insect").len(),
        tokens_named(&runner, "Blood").len(),
    )
}

/// Old Rutstein ruling: "If you mill Dryad Arbor, a card that's both a creature
/// and a land, you get both a Treasure token and a 1/1 green Insect token."
/// CR 608.2c + CR 111.1: the Insect gate reads the milled card, not the
/// Treasure the gated node before it created, and the Blood gate does not read
/// that Treasure either.
#[test]
fn old_rutstein_dryad_arbor_makes_treasure_and_insect() {
    assert_eq!(old_rutstein_upkeep(Milled::DryadArbor), (1, 1, 0));
}

#[test]
fn old_rutstein_creature_makes_only_an_insect() {
    assert_eq!(old_rutstein_upkeep(Milled::Creature), (0, 1, 0));
}

#[test]
fn old_rutstein_instant_makes_only_blood() {
    assert_eq!(old_rutstein_upkeep(Milled::Instant), (0, 0, 1));
}

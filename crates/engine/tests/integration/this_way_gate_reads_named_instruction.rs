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

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
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
                WindgraceHand::DiscardBolt => bolt,
                WindgraceHand::Empty => unreachable!("Empty takes the outer arm"),
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

const STINKWEED_IMP: &str = "Flying\nWhenever this creature deals combat damage to a creature, destroy that creature.\nDredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)";

const NOTION_THIEF: &str = "Flash\nIf an opponent would draw a card except the first one they draw in each of their draw steps, instead that player skips that draw and you draw a card.";

const DAKMOR_SALVAGE: &str = "This land enters tapped.\n{T}: Add {B}.\nDredge 2 (If you would draw a card, you may mill two cards instead. If you do, return this card from your graveyard to your hand.)";

/// Lord Windgrace on P0's battlefield over three filler library cards.
fn windgrace_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Lord Windgrace", "Windgrace", 5, LORD_WINDGRACE)
        .id();
    for n in 0..3 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {n}"), false);
    }
    (scenario, walker)
}

fn activate_plus_two(runner: &mut GameRunner, walker: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate +2");
}

/// The engine's own save/undo/P2P restore pipeline (`PersistedGameState`), not
/// a bare `serde_json::from_str::<GameState>`.
fn round_trip(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

/// How each Dredge offer (CR 702.52a) is answered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DredgeAnswer {
    /// Dredge the first draw; decline any later offer.
    AcceptFirst,
    /// Draw every card normally.
    DeclineAll,
}

/// The option of the pending replacement offer labelled `label`.
fn offer_index(runner: &GameRunner, label: &str) -> usize {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == label)
            .unwrap_or_else(|| panic!("a {label} option is offered: {candidates:?}")),
        other => panic!("expected a replacement offer, got {other:?}"),
    }
}

/// Settle the +2: discard `pick` and answer each Dredge offer per `answer`.
/// `at_first_offer` runs on the runner before the first offer is answered.
/// Returns how many Dredge offers appeared.
fn settle_dredge_offers(
    runner: &mut GameRunner,
    pick: ObjectId,
    answer: DredgeAnswer,
    mut at_first_offer: impl FnMut(&mut GameRunner),
) -> usize {
    let mut offers = 0;
    for _ in 0..64 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DiscardChoice { cards, .. } => {
                assert!(cards.contains(&pick), "the staged pick is offered");
                GameAction::SelectCards { cards: vec![pick] }
            }
            WaitingFor::ReplacementChoice { .. } => {
                offers += 1;
                if offers == 1 {
                    at_first_offer(runner);
                }
                let label = match answer {
                    DredgeAnswer::AcceptFirst if offers == 1 => "Accept",
                    DredgeAnswer::AcceptFirst | DredgeAnswer::DeclineAll => "Decline",
                };
                GameAction::ChooseReplacement {
                    index: offer_index(runner, label),
                }
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
    offers
}

/// Lord Windgrace with Stinkweed Imp in P0's graveyard, five lands (else five
/// sorceries) on top of the library, and Forest (else Bolt) as the only card in
/// hand. Returns (runner, walker, discard pick, Imp).
fn windgrace_imp_board(
    discard_land: bool,
    mill_lands: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (mut scenario, walker) = windgrace_scenario();
    for n in 0..5 {
        if mill_lands {
            scenario.add_land_to_library_top(P0, &format!("Swamp {n}"));
        } else {
            scenario.add_spell_to_library_top(P0, &format!("Sorcery {n}"), false);
        }
    }
    let imp = scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP)
        .id();
    let pick = if discard_land {
        scenario.add_land_to_hand(P0, "Forest").id()
    } else {
        scenario.add_bolt_to_hand(P0)
    };
    (scenario.build(), walker, pick, imp)
}

/// Activate Lord Windgrace's +2 over [`windgrace_imp_board`] and answer each
/// Dredge offer per `answer`. Returns (cards drawn — the library's loss beyond
/// the five milled cards when Dredge was accepted — Dredge offers, Imp's zone).
fn windgrace_plus_two_dredged(
    discard_land: bool,
    mill_lands: bool,
    answer: DredgeAnswer,
) -> (usize, usize, Zone) {
    let (mut runner, walker, pick, imp) = windgrace_imp_board(discard_land, mill_lands);
    let library_before = runner.state().players[0].library.len();
    activate_plus_two(&mut runner, walker);
    let offers = settle_dredge_offers(&mut runner, pick, answer, |_| {});
    assert_eq!(runner.state().objects[&pick].zone, Zone::Graveyard);
    let milled = match answer {
        DredgeAnswer::AcceptFirst => 5,
        DredgeAnswer::DeclineAll => 0,
    };
    let drawn = library_before - runner.state().players[0].library.len() - milled;
    (drawn, offers, runner.state().objects[&imp].zone)
}

/// CR 608.2c + CR 702.52a: when Dredge replaces the draw, the gate still reads
/// the discard, not the milled cards or the returned dredger.
#[test]
fn lord_windgrace_dredged_draw_gate_reads_the_discard() {
    assert_eq!(
        windgrace_plus_two_dredged(false, true, DredgeAnswer::AcceptFirst),
        (0, 1, Zone::Hand),
        "Bolt discarded, lands milled"
    );
    assert_eq!(
        windgrace_plus_two_dredged(true, false, DredgeAnswer::AcceptFirst),
        (1, 1, Zone::Hand),
        "Forest discarded, sorceries milled"
    );
}

/// CR 702.52a: a declined Dredge draws normally, and the Imp still in the
/// graveyard is offered again for the additional draw.
#[test]
fn lord_windgrace_declined_dredge_gate_reads_the_discard() {
    assert_eq!(
        windgrace_plus_two_dredged(true, false, DredgeAnswer::DeclineAll),
        (2, 2, Zone::Graveyard),
        "Forest discarded"
    );
    assert_eq!(
        windgrace_plus_two_dredged(false, true, DredgeAnswer::DeclineAll),
        (1, 1, Zone::Graveyard),
        "Bolt discarded"
    );
}

/// CR 608.2c + CR 614.6: Notion Thief replaces each of P0's draws with its
/// controller's draw, with no choice. The gate still reads the discard, not the
/// card the thief's controller drew. P0 holds Forest and Bolt and discards
/// Forest (else Bolt). Returns (P0 library loss, P1 hand gain).
fn windgrace_plus_two_under_notion_thief(discard_land: bool, p1_top_land: bool) -> (usize, usize) {
    let (mut scenario, walker) = windgrace_scenario();
    scenario.add_creature_from_oracle(P1, "Notion Thief", 3, 1, NOTION_THIEF);
    for n in 0..3 {
        scenario.add_spell_to_library_top(P1, &format!("P1 Filler {n}"), false);
    }
    if p1_top_land {
        scenario.add_land_to_library_top(P1, "P1 Swamp");
    } else {
        scenario.add_spell_to_library_top(P1, "P1 Sorcery", false);
    }
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let pick = if discard_land { forest } else { bolt };
    let mut runner = scenario.build();
    let p0_library_before = runner.state().players[0].library.len();
    let p1_hand_before = runner.state().players[1].hand.len();
    activate_plus_two(&mut runner, walker);
    assert_eq!(settle(&mut runner, Some(pick)), 1, "one discard prompt");
    assert_eq!(runner.state().objects[&pick].zone, Zone::Graveyard);
    (
        p0_library_before - runner.state().players[0].library.len(),
        runner.state().players[1].hand.len() - p1_hand_before,
    )
}

#[test]
fn lord_windgrace_notion_thief_replaced_draw_gate_reads_the_discard() {
    assert_eq!(
        windgrace_plus_two_under_notion_thief(true, false),
        (0, 2),
        "Forest discarded, the thief's controller draws nonland cards"
    );
    assert_eq!(
        windgrace_plus_two_under_notion_thief(false, true),
        (0, 1),
        "Bolt discarded, the thief's controller draws a land"
    );
}

/// CR 608.2c + CR 701.9a: Dakmor Salvage was discarded this way, and dredging
/// it back to hand for the first draw does not undo that discard.
#[test]
fn lord_windgrace_dakmor_salvage_dredged_back_still_counts() {
    let (mut scenario, walker) = windgrace_scenario();
    for n in 0..2 {
        scenario.add_spell_to_library_top(P0, &format!("Sorcery {n}"), false);
    }
    let dakmor = scenario
        .add_land_to_hand(P0, "Dakmor Salvage")
        .from_oracle_text_with_keywords(&["Dredge"], DAKMOR_SALVAGE)
        .id();
    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();
    activate_plus_two(&mut runner, walker);
    let offers = settle_dredge_offers(&mut runner, dakmor, DredgeAnswer::AcceptFirst, |_| {});
    assert_eq!(offers, 1, "Dredge offered once, for the first draw");
    assert_eq!(runner.state().objects[&dakmor].zone, Zone::Hand);
    assert_eq!(
        library_before - runner.state().players[0].library.len() - 2,
        1,
        "the additional card is drawn beyond the two milled"
    );
}

/// The draw instruction's held ledger survives a save/restore taken while the
/// Dredge offer is pending.
#[test]
fn lord_windgrace_dredged_draw_gate_survives_a_state_round_trip() {
    let (mut runner, walker, pick, imp) = windgrace_imp_board(true, false);
    let library_before = runner.state().players[0].library.len();
    activate_plus_two(&mut runner, walker);
    let offers = settle_dredge_offers(&mut runner, pick, DredgeAnswer::AcceptFirst, |runner| {
        *runner = round_trip(runner);
    });
    assert_eq!(offers, 1);
    assert_eq!(runner.state().objects[&imp].zone, Zone::Hand);
    assert_eq!(
        library_before - runner.state().players[0].library.len() - 5,
        1,
        "Forest discarded: one additional card beyond the five milled"
    );
}

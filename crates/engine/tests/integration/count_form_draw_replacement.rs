//! Count-form draw replacements ("if [a player] would draw N or more cards")
//! driven through a real spell's draw (issue #5678, Alms Collector).
//!
//! CR 121.2a: an instruction to draw multiple cards can be modified by
//! replacement effects that refer to the number of cards drawn, before any of
//! the individual draws. A count-form definition is consulted once for the whole
//! instruction and gated on its count; individual-draw replacements (Teferi's
//! Ageless Insight) keep seeing every individual draw, including the draws a
//! count-form substitute performs.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const ALMS_COLLECTOR: &str = "Flash\nIf an opponent would draw two or more cards, instead you and that player each draw a card.";

const TEFERIS_AGELESS_INSIGHT: &str =
    "If you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";

const QUANTUM_RIDDLER_DRAW: &str = "As long as you have one or fewer cards in hand, if you would draw one or more cards, you draw that many cards plus one instead.";

/// A count-form mandatory skip. It takes the early-return `Prevent` parse path,
/// which must still carry the ">= N" threshold.
const COUNT_FORM_SKIP: &str =
    "If an opponent would draw two or more cards, skip that draw instead.";

fn hand(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn library(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

/// P0 in their precombat main with a draw spell in hand and mana to cast it,
/// every player's library stocked. `setup` places the permanents under test.
/// Returns the runner once the spell has resolved (or halted at a prompt the
/// driver does not answer) and the spell's id.
fn cast_draw_spell_in(
    mut scenario: GameScenario,
    players: &[PlayerId],
    draw_oracle: &str,
    setup: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    for &player in players {
        for i in 0..10 {
            scenario.add_spell_to_library_top(player, &format!("Filler {i}"), true);
        }
    }
    setup(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Draw Spell", false, draw_oracle)
        .id();
    let mana: Vec<ManaUnit> = (0..8)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    (runner, spell)
}

fn cast_draw_spell(draw_oracle: &str, setup: impl FnOnce(&mut GameScenario)) -> GameRunner {
    cast_draw_spell_in(GameScenario::new(), &[P0, P1], draw_oracle, setup).0
}

fn add_alms_collector(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(controller, "Alms Collector", 3, 3, ALMS_COLLECTOR)
        .id()
}

fn add_teferi(scenario: &mut GameScenario, controller: PlayerId) {
    scenario
        .add_creature_from_oracle(
            controller,
            "Teferi's Ageless Insight",
            0,
            0,
            TEFERIS_AGELESS_INSIGHT,
        )
        .as_enchantment();
}

#[test]
fn alms_collector_ignores_an_opponents_single_card_draw() {
    let runner = cast_draw_spell("Draw a card.", |scenario| {
        add_alms_collector(scenario, P1);
    });

    assert_eq!(
        hand(&runner, P0),
        1,
        "a one-card draw is below the threshold"
    );
    assert_eq!(
        hand(&runner, P1),
        0,
        "Alms Collector's controller draws nothing"
    );
}

#[test]
fn alms_collector_replaces_an_opponents_two_card_draw() {
    let runner = cast_draw_spell("Draw two cards.", |scenario| {
        add_alms_collector(scenario, P1);
    });

    assert_eq!(
        hand(&runner, P0),
        1,
        "the opponent's two-card draw becomes one card for that player"
    );
    assert_eq!(
        hand(&runner, P1),
        1,
        "Alms Collector's controller draws the other card"
    );
    assert_eq!(library(&runner, P0), 9);
    assert_eq!(library(&runner, P1), 9);
}

#[test]
fn individual_draw_replacements_see_the_count_form_substitutes_draws() {
    // CR 121.2a + CR 614.6: Alms Collector replaces P0's two-card instruction.
    // The draws it performs instead are individual draws, so each player's
    // Teferi's Ageless Insight doubles its own player's substitute draw.
    let runner = cast_draw_spell("Draw two cards.", |scenario| {
        add_alms_collector(scenario, P1);
        add_teferi(scenario, P0);
        add_teferi(scenario, P1);
    });

    assert_eq!(
        hand(&runner, P0),
        2,
        "the drawing opponent's substitute draw is doubled by their Teferi"
    );
    assert_eq!(
        hand(&runner, P1),
        2,
        "Alms Collector's controller's substitute draw is doubled by their Teferi"
    );
}

#[test]
fn count_form_skip_prevents_only_draws_at_or_above_the_threshold() {
    let single = cast_draw_spell("Draw a card.", |scenario| {
        scenario.add_creature_from_oracle(P1, "Skip Source", 1, 1, COUNT_FORM_SKIP);
    });
    assert_eq!(
        hand(&single, P0),
        1,
        "a one-card draw is below the skip's threshold"
    );

    let double = cast_draw_spell("Draw two cards.", |scenario| {
        scenario.add_creature_from_oracle(P1, "Skip Source", 1, 1, COUNT_FORM_SKIP);
    });
    assert_eq!(hand(&double, P0), 0, "the two-card draw is skipped");
    assert_eq!(library(&double, P0), 10);
}

#[test]
fn third_player_chooses_which_alms_collector_replaces_their_draw() {
    // Alms Collector ruling (2017-08-25): if two players each control an Alms
    // Collector and a third player would draw two or more cards, the third player
    // chooses which one applies. CR 616.1: the affected player chooses; the
    // instruction's consult parks on that choice and the choice settles it.
    let mut alms = Vec::new();
    let (mut runner, spell) = cast_draw_spell_in(
        GameScenario::new_n_player(3, 42),
        &[P0, P1, P2],
        "Draw two cards.",
        |scenario| {
            alms.push(add_alms_collector(scenario, P1));
            alms.push(add_alms_collector(scenario, P2));
        },
    );
    let p1_alms = alms[0];

    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected P0 to choose between the two Alms Collectors, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the drawing player chooses");
    assert_eq!(candidates.len(), 2);
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == p1_alms)
        .expect("P1's Alms Collector is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("choose P1's Alms Collector");
    runner.advance_until_stack_empty();

    assert_eq!(
        hand(&runner, P0),
        1,
        "the drawer gets the substitute's card"
    );
    assert_eq!(
        hand(&runner, P1),
        1,
        "the chosen Alms Collector's controller draws"
    );
    assert_eq!(
        hand(&runner, P2),
        0,
        "the other Alms Collector does not apply to the replaced instruction"
    );
    let state = runner.state();
    assert!(
        state.resolution_stack.is_empty() && state.resolving_stack_entry.is_none(),
        "the parked draw instruction must settle and complete"
    );
    assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn quantum_riddler_modifies_the_instruction_before_teferis_individual_draws() {
    // CR 121.2a: Quantum Riddler applies to the two-card instruction (two become
    // three) before any individual draw, and Teferi's Ageless Insight then
    // doubles each of the three individual draws. They never compete for the
    // same event, so no CR 616.1 ordering choice arises.
    let runner = cast_draw_spell("Draw two cards.", |scenario| {
        scenario.add_creature_from_oracle(P0, "Quantum Riddler", 4, 6, QUANTUM_RIDDLER_DRAW);
        add_teferi(scenario, P0);
    });

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the instruction and individual stages each have one replacement"
    );
    assert_eq!(hand(&runner, P0), 6);
    assert_eq!(library(&runner, P0), 4);
}

//! "This way" readers after a per-player fan-out whose seats pause for a choice
//! (CR 608.2c, CR 608.2e), and the "for each <noun> less/fewer than N" shortfall
//! count (CR 107.1b).
//!
//! A `player_scope` fan-out that pauses on a seat's choice parks the remaining
//! seats and the unscoped tail as a continuation. The paused seat's producer,
//! resumed by the answer, has no reader in its own chain; it must still publish
//! into the clause's tracked set so the tail's "this way" count sees every
//! seat's results. A seat or instruction that chooses its counter recipient
//! while resolving (CR 608.2d) extends that set too, rather than replacing it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const LIBRARY: [&str; 4] = ["Library 1", "Library 2", "Library 3", "Library 4"];

/// Synthetic composition (not a printed card) of the class's two halves: a
/// per-player instruction that pauses every seat on a choose-one prompt, then a
/// controller-only "this way" count over every seat's results (the shape of
/// Eumidian Wastewaker's "you and defending player each discard a card or
/// sacrifice a permanent. You draw a card for each land card put into a
/// graveyard this way.").
const FAN_OUT_DISCARD_OR_SACRIFICE: &str = "Each player discards a card or sacrifices a permanent. You draw a card for each land card put into a graveyard this way.";

const AGITATOR_ANT: &str = "At the beginning of your end step, each player may put two +1/+1 counters on a creature they control. Goad each creature that had counters put on it this way. (Until your next turn, those creatures attack each combat if able and attack a player other than you if able.)";

const STABWHISKER_THE_ODIOUS: &str = "At the beginning of each opponent's upkeep, that player loses 1 life for each card fewer than three in their hand.";

const KATHRIL: &str = "When Kathril enters, put a flying counter on any creature you control if a creature card in your graveyard has flying. Repeat this process for first strike, double strike, deathtouch, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance. Then put a +1/+1 counter on Kathril for each counter put on a creature this way.";

const TRUCE: &str = "Each player may draw up to two cards. For each card less than two a player draws this way, that player gains 2 life.";

fn scenario_for(players: u8) -> GameScenario {
    if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 7)
    }
}

/// Each player `i` holds only `discards[i]` (P0 also holds the spell). Every
/// seat chooses the discard branch; returns the number of cards P0 drew.
fn fan_out_discard_draws(discards: &[&str]) -> usize {
    let players = u8::try_from(discards.len()).expect("small seat count");
    let mut scenario = scenario_for(players);
    scenario.at_phase(Phase::PreCombatMain);
    let mut discarded = Vec::new();
    for (seat, &card) in (0..players).map(PlayerId).zip(discards) {
        scenario.with_library_top(seat, &LIBRARY);
        let id = if card == "Grizzly Bears" {
            scenario.add_card_to_hand(seat, card)
        } else {
            scenario.add_land_to_hand(seat, card).id()
        };
        discarded.push(id);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fan-Out", false, FAN_OUT_DISCARD_OR_SACRIFICE)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();

    let mut branch_prompts = Vec::new();
    for _ in 0..32 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::ChooseOneOfBranch { player, .. } => {
                branch_prompts.push(*player);
                GameAction::ChooseBranch { index: 0 }
            }
            WaitingFor::DiscardChoice { cards, count, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }

    // Reach-guard: every seat paused on its own choice, in APNAP order
    // (CR 101.4), and every seat's card was discarded.
    assert_eq!(
        branch_prompts,
        (0..players).map(PlayerId).collect::<Vec<_>>(),
        "each seat pauses on its choose-one prompt"
    );
    for id in &discarded {
        assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
    }
    runner.state().players[0].hand.len()
}

/// CR 608.2c + CR 608.2e: two seats, both discard a land; P0 draws two.
#[test]
fn two_seat_pause_counts_every_seat_this_way() {
    assert_eq!(fan_out_discard_draws(&["Forest", "Mountain"]), 2);
}

/// CR 608.2c + CR 608.2e: three seats, every seat discards a land; P0 draws three.
#[test]
fn three_seat_pause_counts_every_seat_this_way() {
    assert_eq!(fan_out_discard_draws(&["Forest", "Mountain", "Plains"]), 3);
}

/// CR 608.2c + CR 608.2e: three seats, the middle seat discards a nonland card;
/// only the two lands count.
#[test]
fn three_seat_pause_counts_only_matching_cards() {
    assert_eq!(
        fan_out_discard_draws(&["Forest", "Grizzly Bears", "Mountain"]),
        2
    );
}

/// The land filter's reach-guard: the head seat's nonland discard is not
/// counted.
#[test]
fn two_seat_pause_skips_nonland_head_seat() {
    assert_eq!(fan_out_discard_draws(&["Grizzly Bears", "Mountain"]), 1);
}

/// Agitator Ant on P0's battlefield as P0's only creature; P1 and P2 each
/// control one Grizzly Bears. Resolves the end-step trigger, answering each
/// seat's optional prompt with `accept[seat]`, and returns each creature's
/// (+1/+1 counters, goaded by P0) in seat order.
fn agitator_ant_end_step(accept: [bool; 3]) -> Vec<(u32, bool)> {
    agitator_ant_end_step_with_bears(accept, [1, 1])
}

/// As [`agitator_ant_end_step`], but P1 and P2 control `bears[0]` and
/// `bears[1]` Grizzly Bears. A seat with two or more creatures chooses at
/// resolution (CR 608.2d) and picks its first Bears. Returns the Ant, then P1's
/// Bears, then P2's Bears, each in creation order.
fn agitator_ant_end_step_with_bears(accept: [bool; 3], bears: [usize; 2]) -> Vec<(u32, bool)> {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PostCombatMain);
    let ant = scenario
        .add_creature_from_oracle(P0, "Agitator Ant", 2, 2, AGITATOR_ANT)
        .id();
    let bears_by_seat: Vec<Vec<ObjectId>> = [P1, P2]
        .into_iter()
        .zip(bears)
        .map(|(seat, count)| {
            (0..count)
                .map(|_| scenario.add_creature(seat, "Grizzly Bears", 2, 2).id())
                .collect()
        })
        .collect();
    let mut runner = scenario.build();

    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the end-step trigger is on the stack"
    );

    // CR 101.4: each player's choice is made in APNAP order.
    let mut optional_prompts = Vec::new();
    let mut creature_prompts = Vec::new();
    for _ in 0..32 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { player, .. } => {
                optional_prompts.push(*player);
                GameAction::DecideOptionalEffect {
                    accept: accept[usize::from(player.0)],
                }
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                creature_prompts.push(*player);
                let first_bears = bears_by_seat[usize::from(player.0) - 1][0];
                assert!(cards.contains(&first_bears), "the seat's Bears is offered");
                GameAction::SelectCards {
                    cards: vec![first_bears],
                }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }
    assert_eq!(optional_prompts, vec![P0, P1, P2]);
    // Reach-guard: each accepting seat with two or more creatures chose one.
    let choosing_seats: Vec<PlayerId> = [P1, P2]
        .into_iter()
        .zip(bears)
        .filter(|&(seat, count)| accept[usize::from(seat.0)] && count >= 2)
        .map(|(seat, _)| seat)
        .collect();
    assert_eq!(creature_prompts, choosing_seats);

    std::iter::once(ant)
        .chain(bears_by_seat.into_iter().flatten())
        .map(|id| {
            let creature = &runner.state().objects[&id];
            (
                creature
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                creature.goaded_by.contains(&P0),
            )
        })
        .collect()
}

/// CR 608.2c + CR 701.15a: every player accepts, so every creature gets two
/// counters and is goaded, including the head seat's Ant.
#[test]
fn agitator_ant_all_accept_goads_every_creature() {
    assert_eq!(
        agitator_ant_end_step([true, true, true]),
        vec![(2, true), (2, true), (2, true)]
    );
}

/// CR 608.2c + CR 701.15a: P1 declines; P1's Bears has neither counters nor
/// goad, while the accepting seats' creatures are goaded.
#[test]
fn agitator_ant_decliner_is_not_goaded() {
    assert_eq!(
        agitator_ant_end_step([true, false, true]),
        vec![(2, true), (0, false), (2, true)]
    );
}

/// CR 608.2c + CR 701.15a: the head seat declines and contributes nothing; the
/// other seats' creatures are goaded.
#[test]
fn agitator_ant_head_seat_declines() {
    assert_eq!(
        agitator_ant_end_step([false, true, true]),
        vec![(0, false), (2, true), (2, true)]
    );
}

/// CR 608.2c + CR 608.2d + CR 701.15a: P1 controls two creatures and chooses
/// one at resolution. Every chosen creature is goaded, including the head
/// seat's Ant; the creature P1 did not choose gets nothing.
#[test]
fn agitator_ant_middle_seat_chooses_among_two_creatures() {
    assert_eq!(
        agitator_ant_end_step_with_bears([true, true, true], [2, 1]),
        vec![(2, true), (2, true), (0, false), (2, true)]
    );
}

/// CR 608.2c + CR 608.2d + CR 701.15a: the last seat, P2, controls two
/// creatures and chooses one at resolution; the earlier seats' creatures are
/// still goaded.
#[test]
fn agitator_ant_last_seat_chooses_among_two_creatures() {
    assert_eq!(
        agitator_ant_end_step_with_bears([true, true, true], [1, 2]),
        vec![(2, true), (2, true), (2, true), (0, false)]
    );
}

/// Stabwhisker the Odious on P0's battlefield; P1 holds `hand` cards at the
/// beginning of their upkeep. Returns the life P1 lost.
fn stabwhisker_life_loss(hand: usize) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_library_top(P1, &LIBRARY);
    scenario.add_creature_from_oracle(P0, "Stabwhisker the Odious", 3, 3, STABWHISKER_THE_ODIOUS);
    for i in 0..hand {
        scenario.add_card_to_hand(P1, &format!("Hand {i}"));
    }
    let mut runner = scenario.build();
    let before = runner.life(P1);

    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().players[1].hand.len(), hand);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the upkeep trigger is on the stack"
    );
    runner.advance_until_stack_empty();
    before - runner.life(P1)
}

/// CR 107.1b + CR 608.2c: "for each card fewer than three in their hand" is
/// three minus the hand size, and nothing once the hand holds three or more.
#[test]
fn stabwhisker_counts_cards_fewer_than_three() {
    assert_eq!(stabwhisker_life_loss(0), 3);
    assert_eq!(stabwhisker_life_loss(1), 2);
    assert_eq!(stabwhisker_life_loss(4), 0);
}

/// CR 107.1b + CR 121.1: when every player draws two, no player draws fewer
/// than two, so nobody gains life — even though the clause-wide "this way" set
/// now holds every player's draws.
#[test]
fn truce_everyone_draws_two_gains_no_life() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in [P0, P1, P2] {
        scenario.with_library_top(seat, &LIBRARY);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Truce", true, TRUCE)
        .id();
    let mut runner = scenario.build();
    let life_before: Vec<i32> = [P0, P1, P2].map(|seat| runner.life(seat)).to_vec();
    runner.cast(spell).commit();

    for _ in 0..32 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => GameAction::ChooseBranch {
                index: branch_descriptions.len() - 1,
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }

    // Reach-guard: every player drew two cards this way.
    for seat in [P0, P1, P2] {
        assert_eq!(runner.state().players[usize::from(seat.0)].hand.len(), 2);
    }
    assert_eq!(
        [P0, P1, P2].map(|seat| runner.life(seat)).to_vec(),
        life_before
    );
}

/// Kathril with a flying and a first strike creature card in P0's graveyard
/// and Grizzly Bears beside it: two of its counter instructions choose a
/// recipient while resolving (CR 608.2d). The flying counter goes on the Bears
/// and the first strike counter on Kathril, so two counters were put on
/// creatures this way and Kathril gets two +1/+1 counters (CR 608.2c). The
/// second answer must not replace the set the first instruction published.
///
/// Only the distinct-recipient reading is asserted. The count reads the
/// creatures in the set, not the counters, so two counters on one creature
/// still give one +1/+1 counter.
#[test]
fn kathril_counts_each_resolution_chosen_recipient_this_way() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_to_graveyard(P0, "Skybound Remains", 1, 1)
        .flying();
    scenario
        .add_creature_to_graveyard(P0, "Duelist Remains", 1, 1)
        .first_strike();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let kathril = scenario
        .add_creature_to_hand_from_oracle(P0, "Kathril, Aspect Warper", 3, 3, KATHRIL)
        .id();
    // Kathril costs {2}{W}{B}{G}.
    scenario.with_mana_pool(
        P0,
        [
            ManaType::White,
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .zip(9_996u64..)
        .map(|(mana, id)| ManaUnit::new(mana, ObjectId(id), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(kathril).commit();

    let mut recipients = [bears, kathril].into_iter();
    let mut recipient_prompts = 0;
    for _ in 0..32 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::ChooseFromZoneChoice { .. } => {
                recipient_prompts += 1;
                GameAction::SelectCards {
                    cards: vec![recipients.next().expect("two recipient prompts")],
                }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("action accepted");
    }

    let counters = |id: ObjectId, counter: CounterType| {
        runner.state().objects[&id]
            .counters
            .get(&counter)
            .copied()
            .unwrap_or(0)
    };
    // Reach-guard: both instructions prompted and placed their counter.
    assert_eq!(recipient_prompts, 2);
    assert_eq!(
        counters(bears, CounterType::Keyword(Keyword::Flying.kind())),
        1
    );
    assert_eq!(
        counters(kathril, CounterType::Keyword(Keyword::FirstStrike.kind())),
        1
    );
    assert_eq!(counters(kathril, CounterType::Plus1Plus1), 2);
}

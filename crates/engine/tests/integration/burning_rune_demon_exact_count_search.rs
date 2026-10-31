//! Burning-Rune Demon: "exactly two cards not named Burning-Rune Demon" is a
//! fixed count of two with the name filter kept (CR 701.23b, CR 701.23d).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text.
const BURNING_RUNE_DEMON: &str = "Flying\nWhen this creature enters, you may search your \
library for exactly two cards not named Burning-Rune Demon that have different names. If you \
do, reveal those cards. An opponent chooses one of them. Put the chosen card into your hand and \
the other into your graveyard, then shuffle.";

struct Library {
    other_demon: ObjectId,
    a1: ObjectId,
    a2: ObjectId,
    b: ObjectId,
}

impl Library {
    fn all(&self) -> [ObjectId; 4] {
        [self.other_demon, self.a1, self.a2, self.b]
    }
}

/// Casts the demon and passes priority until its optional ETB trigger asks P0.
fn cast_to_optional() -> (GameRunner, Library) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let other_demon = scenario.add_card_to_library_top(P0, "Burning-Rune Demon");
    let a1 = scenario.add_card_to_library_top(P0, "Card A");
    let a2 = scenario.add_card_to_library_top(P0, "Card A");
    let b = scenario.add_card_to_library_top(P0, "Card B");
    let demon = scenario
        .add_creature_to_hand_from_oracle(P0, "Burning-Rune Demon", 6, 6, BURNING_RUNE_DEMON)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(demon).commit();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "expected the optional search prompt, got {:?}",
        runner.state().waiting_for
    );
    (
        runner,
        Library {
            other_demon,
            a1,
            a2,
            b,
        },
    )
}

/// Accepts the optional search and checks the SearchChoice prompt it opens.
fn accept_to_search() -> (GameRunner, Library, Vec<GameEvent>) {
    let (mut runner, library) = cast_to_optional();
    let events = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional search")
        .events;
    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        up_to,
        allows_partial_find,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(count, 2);
    assert!(!up_to);
    // CR 701.23b: "not named Burning-Rune Demon" is a stated quality.
    assert!(allows_partial_find);
    let mut offered = cards.clone();
    offered.sort();
    let mut expected = vec![library.a1, library.a2, library.b];
    expected.sort();
    assert_eq!(offered, expected);
    (runner, library, events)
}

/// Submits P0's search selection and checks P1's choice prompt offers exactly it.
fn select_to_opponent_choice(runner: &mut GameRunner, selected: &[ObjectId]) -> Vec<GameEvent> {
    let events = runner
        .act(GameAction::SelectCards {
            cards: selected.to_vec(),
        })
        .expect("search selection resumes the trigger")
        .events;
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected ChooseFromZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert_eq!(count, 1);
    let mut offered = cards;
    offered.sort();
    let mut expected = selected.to_vec();
    expected.sort();
    assert_eq!(offered, expected);
    events
}

fn shuffled(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id: P0,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )
    })
}

fn assert_zone(runner: &GameRunner, ids: &[ObjectId], zone: Zone) {
    for id in ids {
        assert_eq!(runner.state().objects[id].zone, zone, "{id:?}");
    }
}

fn assert_resolved(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn chosen_card_goes_to_hand_and_the_other_to_graveyard() {
    let (mut runner, library, mut events) = accept_to_search();
    events.extend(select_to_opponent_choice(
        &mut runner,
        &[library.a1, library.b],
    ));
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![library.b],
            })
            .expect("opponent's choice resumes the trigger")
            .events,
    );
    assert_zone(&runner, &[library.b], Zone::Hand);
    assert_zone(&runner, &[library.a1], Zone::Graveyard);
    assert_zone(&runner, &[library.a2, library.other_demon], Zone::Library);
    assert!(shuffled(&events));
    assert_resolved(&runner);
}

#[test]
fn finding_one_card_puts_it_into_hand() {
    let (mut runner, library, _) = accept_to_search();
    let graveyard_before = runner.state().players[P0.0 as usize].graveyard.clone();
    select_to_opponent_choice(&mut runner, &[library.b]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![library.b],
        })
        .expect("opponent's choice resumes the trigger");
    assert_zone(&runner, &[library.b], Zone::Hand);
    assert_zone(&runner, &[library.a1, library.a2], Zone::Library);
    assert_eq!(
        runner.state().players[P0.0 as usize].graveyard,
        graveyard_before
    );
    assert_resolved(&runner);
}

#[test]
fn two_cards_with_the_same_name_are_refused() {
    let (mut runner, library, _) = accept_to_search();
    assert!(runner
        .act(GameAction::SelectCards {
            cards: vec![library.a1, library.a2],
        })
        .is_err());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SearchChoice { .. }
    ));
}

#[test]
fn declining_moves_nothing_and_does_not_shuffle() {
    let (mut runner, library) = cast_to_optional();
    let events = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional search")
        .events;
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::SearchChoice { .. }
    ));
    assert_zone(&runner, &library.all(), Zone::Library);
    assert!(!shuffled(&events));
    assert_resolved(&runner);
}

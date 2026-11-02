//! Gifts Given: the targeted opponent chooses which revealed cards go to their
//! graveyard; the rest go to their hand (CR 400.3), and they shuffle.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text.
const GIFTS_GIVEN: &str = "Search target opponent's library for four cards with different \
names and reveal them. That player chooses two of those cards. Put the chosen cards into the \
player's graveyard and the rest into your hand. Then that player shuffles their library.";

/// Verbatim Scryfall Oracle text.
const GIFTS_UNGIVEN: &str = "Search your library for up to four cards with different names \
and reveal them. Target opponent chooses two of those cards. Put the chosen cards into your \
graveyard and the rest into your hand. Then shuffle.";

const LIBRARY: [&str; 5] = ["Card A", "Card B", "Card C", "Card D", "Card E"];

struct Resolved {
    runner: GameRunner,
    found: Vec<ObjectId>,
    chosen: Vec<ObjectId>,
    rest: Vec<ObjectId>,
    shuffled: Vec<PlayerId>,
}

/// Casts `oracle` from P0 targeting `target` and returns the cards P0's
/// search offers from `library_owner`'s library.
fn cast_to_search(
    mut scenario: GameScenario,
    name: &str,
    oracle: &str,
    library_owner: PlayerId,
    target: PlayerId,
) -> (GameRunner, Vec<ObjectId>) {
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(library_owner, &LIBRARY);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_player(target).resolve();

    let WaitingFor::SearchChoice {
        player,
        library_owner: searched,
        cards,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(searched, Some(library_owner));
    assert_eq!(count, 4);
    (runner, cards)
}

fn hand_sizes(runner: &GameRunner) -> Vec<usize> {
    runner
        .state()
        .players
        .iter()
        .map(|player| player.hand.len())
        .collect()
}

fn zone_changes_of<'a>(
    events: &'a [GameEvent],
    ids: &'a [ObjectId],
) -> impl Iterator<Item = (Option<Zone>, Zone)> + 'a {
    events.iter().filter_map(move |event| match event {
        GameEvent::ZoneChanged {
            object_id,
            from,
            to,
            ..
        } if ids.contains(object_id) => Some((*from, *to)),
        _ => None,
    })
}

fn shuffled_players(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect()
}

/// Casts `oracle` from P0 targeting `target`, has P0 find the first four
/// cards of `library_owner`'s library, and has `expected_chooser` pick
/// `found[pick.0]` and `found[pick.1]`.
fn cast_and_choose(
    scenario: GameScenario,
    name: &str,
    oracle: &str,
    library_owner: PlayerId,
    target: PlayerId,
    expected_chooser: PlayerId,
    pick: (usize, usize),
) -> Resolved {
    let (mut runner, cards) = cast_to_search(scenario, name, oracle, library_owner, target);
    let hands_at_search = hand_sizes(&runner);
    let found: Vec<_> = cards.into_iter().take(4).collect();
    let mut events = runner
        .act(GameAction::SelectCards {
            cards: found.clone(),
        })
        .expect("search selection resumes the spell")
        .events;

    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards: offered,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected ChooseFromZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, expected_chooser);
    assert_eq!(count, 2);
    let mut offered_sorted = offered.clone();
    offered_sorted.sort();
    let mut found_sorted = found.clone();
    found_sorted.sort();
    assert_eq!(offered_sorted, found_sorted);
    // CR 701.20b + CR 701.24b: the found cards wait, revealed, in the library.
    let owner_library = &runner.state().players[library_owner.0 as usize].library;
    for id in &found {
        assert_eq!(runner.state().objects[id].zone, Zone::Library, "{id:?}");
        assert!(owner_library.contains(id), "{id:?}");
    }
    assert_eq!(hand_sizes(&runner), hands_at_search);
    assert!(zone_changes_of(&events, &found).all(|(_, to)| to != Zone::Hand));

    let chosen = vec![found[pick.0], found[pick.1]];
    let rest: Vec<_> = found
        .iter()
        .copied()
        .filter(|id| !chosen.contains(id))
        .collect();
    let choice_events = runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("the choice resumes the spell")
        .events;
    let moves: Vec<_> = zone_changes_of(&choice_events, &found).collect();
    assert_eq!(moves.len(), found.len());
    assert!(moves.iter().all(|(from, _)| *from == Some(Zone::Library)));
    events.extend(choice_events);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let shuffled = shuffled_players(&events);
    Resolved {
        runner,
        found,
        chosen,
        rest,
        shuffled,
    }
}

fn assert_in_zone(runner: &GameRunner, ids: &[ObjectId], owner: PlayerId, zone: Zone) {
    let player = &runner.state().players[owner.0 as usize];
    for id in ids {
        assert_eq!(runner.state().objects[id].zone, zone, "{id:?}");
        let held = match zone {
            Zone::Graveyard => player.graveyard.contains(id),
            Zone::Hand => player.hand.contains(id),
            other => panic!("unexpected zone {other:?}"),
        };
        assert!(held, "{id:?} not in {owner:?}'s {zone:?}");
    }
}

#[test]
fn gifts_given_targeted_opponent_chooses_and_cards_stay_with_their_owner() {
    let r = cast_and_choose(
        GameScenario::new(),
        "Gifts Given",
        GIFTS_GIVEN,
        P1,
        P1,
        P1,
        (3, 1),
    );
    assert_in_zone(&r.runner, &r.chosen, P1, Zone::Graveyard);
    assert_in_zone(&r.runner, &r.rest, P1, Zone::Hand);
    let p0 = &r.runner.state().players[0];
    assert!(r.found.iter().all(|id| !p0.hand.contains(id)));
    assert_eq!(r.shuffled, vec![P1]);
}

#[test]
fn gifts_given_binds_the_announced_opponent_among_several() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.with_library_top(P1, &["Bystander card"]);
    let r = cast_and_choose(scenario, "Gifts Given", GIFTS_GIVEN, p2, p2, p2, (0, 2));
    assert_in_zone(&r.runner, &r.chosen, p2, Zone::Graveyard);
    assert_in_zone(&r.runner, &r.rest, p2, Zone::Hand);
    assert_eq!(r.runner.state().players[1].library.len(), 1);
    assert_eq!(r.shuffled, vec![p2]);
}

#[test]
fn gifts_ungiven_partition_routes_the_casters_cards() {
    let r = cast_and_choose(
        GameScenario::new(),
        "Gifts Ungiven",
        GIFTS_UNGIVEN,
        P0,
        P1,
        P1,
        (0, 2),
    );
    assert_in_zone(&r.runner, &r.chosen, P0, Zone::Graveyard);
    assert_in_zone(&r.runner, &r.rest, P0, Zone::Hand);
    assert_eq!(r.shuffled, vec![P0]);
}

#[test]
fn gifts_ungiven_finding_nothing_skips_the_choice() {
    let (mut runner, _) =
        cast_to_search(GameScenario::new(), "Gifts Ungiven", GIFTS_UNGIVEN, P0, P1);
    let p0_hand = runner.state().players[0].hand.len();
    let events = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("an empty find resumes the spell")
        .events;
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let p0 = &runner.state().players[0];
    assert_eq!(p0.library.len(), LIBRARY.len());
    assert_eq!(p0.hand.len(), p0_hand);
    assert_eq!(shuffled_players(&events), vec![P0]);
}

/// Verbatim Scryfall Oracle text.
const THREATS_UNDETECTED: &str = "Search your library for up to four creature cards with \
different powers and reveal them. An opponent chooses two of those cards. Shuffle the chosen \
cards into your library and put the rest into your hand.";

#[test]
fn threats_undetected_chosen_cards_stay_in_the_library_through_one_shuffle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creatures: Vec<ObjectId> = (1..=5)
        .map(|power| {
            scenario
                .add_spell_to_library_top(P0, &format!("Power {power}"), false)
                .as_creature()
                .id()
        })
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Threats Undetected", false, THREATS_UNDETECTED)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    for (id, power) in creatures.iter().zip(1..) {
        let obj = runner.state_mut().objects.get_mut(id).unwrap();
        obj.power = Some(power);
        obj.base_power = Some(power);
        obj.toughness = Some(1);
        obj.base_toughness = Some(1);
    }
    runner.cast(spell).resolve();

    let WaitingFor::SearchChoice { cards, count, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(count, 4);
    let found: Vec<_> = cards.into_iter().take(4).collect();
    let mut events = runner
        .act(GameAction::SelectCards {
            cards: found.clone(),
        })
        .expect("search selection resumes the spell")
        .events;
    let WaitingFor::ChooseFromZoneChoice { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected ChooseFromZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);

    let chosen = vec![found[0], found[2]];
    let rest = vec![found[1], found[3]];
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: chosen.clone(),
            })
            .expect("the choice resumes the spell")
            .events,
    );
    assert!(runner.state().stack.is_empty());

    let p0 = &runner.state().players[0];
    for id in &chosen {
        assert_eq!(runner.state().objects[id].zone, Zone::Library, "{id:?}");
        assert!(p0.library.contains(id), "{id:?}");
    }
    assert_in_zone(&runner, &rest, P0, Zone::Hand);
    assert_eq!(
        zone_changes_of(&events, &rest).collect::<Vec<_>>(),
        vec![(Some(Zone::Library), Zone::Hand); 2]
    );
    assert_eq!(zone_changes_of(&events, &chosen).count(), 0, "{events:?}");
    assert_eq!(shuffled_players(&events), vec![P0]);
}

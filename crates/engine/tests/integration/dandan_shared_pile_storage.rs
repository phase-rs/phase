//! Dandân shared library and graveyard: canonical-seat storage, the pool
//! resolver, deck provisioning and the boot guard. Assertions are pile-side;
//! which hand holds a dealt or drawn card belongs to the hand-entry rebind.

use std::collections::BTreeMap;

use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{
    dandan_fixed_deck_names, load_and_hydrate_decks, momir_fixed_deck_names, DeckPayload,
};
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::{library, zones};
use engine::types::ability::AbilityTag;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::format::{DeckSizeRule, FormatConfig};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

use crate::source_census::code_lines;
use crate::support::shared_card_db;

const WASM_LIB_RS: &str = include_str!("../../../engine-wasm/src/lib.rs");

/// The decklist as printed in the format announcement, written out
/// independently of `DANDAN_DECKLIST`.
fn expected_decklist() -> BTreeMap<String, usize> {
    let mut list: BTreeMap<String, usize> = [
        ("Dandân", 10),
        ("Island", 20),
        ("Memory Lapse", 8),
        ("Accumulated Knowledge", 4),
    ]
    .into_iter()
    .map(|(name, count)| (name.to_string(), count))
    .collect();
    for name in [
        "Magical Hack",
        "Mystic Sanctuary",
        "Brainstorm",
        "Capture of Jingzhou",
        "Chart a Course",
        "Control Magic",
        "Crystal Spray",
        "Day's Undoing",
        "Mental Note",
        "Metamorphose",
        "Predict",
        "Telling Time",
        "Unsubstantiate",
        "Halimar Depths",
        "Haunted Fengraf",
        "Lonely Sandbar",
        "Remote Isle",
        "The Surgical Bay",
        "Svyelunite Temple",
    ] {
        list.insert(name.to_string(), 2);
    }
    list
}

fn names_in(state: &GameState, ids: impl IntoIterator<Item = ObjectId>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for id in ids {
        *counts.entry(state.objects[&id].name.clone()).or_insert(0) += 1;
    }
    counts
}

fn loaded_dandan(db: &CardDatabase, seed: u64) -> GameState {
    let mut state = GameState::new(FormatConfig::dandan(), 2, seed);
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
    state
}

fn ids(v: &im::Vector<ObjectId>) -> Vec<ObjectId> {
    v.iter().copied().collect()
}

/// Every object id appears in at most one library or graveyard container.
fn assert_no_id_in_two_containers(state: &GameState) {
    let mut seen = std::collections::HashSet::new();
    for player in &state.players {
        for id in player.library.iter().chain(player.graveyard.iter()) {
            assert!(seen.insert(*id), "{id:?} sits in two containers");
        }
    }
}

#[test]
fn v1_boot_loads_one_pile_into_the_canonical_seat() {
    let Some(db) = shared_card_db() else { return };
    let state = loaded_dandan(db, 7);

    assert_eq!(state.canonical_seat(), P0);
    assert_eq!(state.players[0].library.len(), 80);
    assert!(state.players[1].library.is_empty());
    assert_eq!(ids(state.library_of(P0)), ids(state.library_of(P1)));
    assert_eq!(
        names_in(&state, state.library_of(P0).iter().copied()),
        expected_decklist(),
        "the pile is exactly the announced decklist"
    );
    assert_eq!(state.deck_pools.len(), 1);
    assert_eq!(state.deck_pools[0].player, P0);
    assert!(
        state.seats_with_empty_library().is_empty(),
        "the boot guard admits the seat whose library is the shared pile"
    );
}

#[test]
fn v1_pile_stays_in_the_lowest_seat_when_another_seat_starts() {
    let Some(db) = shared_card_db() else { return };
    let mut state = loaded_dandan(db, 7);
    let _ = start_game_with_starting_player(&mut state, P1);

    assert_eq!(
        state.seat_order,
        vec![P1, P0],
        "reach: the seat order rotated"
    );
    assert_eq!(state.canonical_seat(), P0);
    assert!(state.players[1].library.is_empty());
    assert!(!state.players[0].library.is_empty());
}

#[test]
fn v1_non_shared_formats_keep_per_seat_libraries() {
    let Some(db) = shared_card_db() else { return };
    let snow_basics: std::collections::BTreeSet<String> = [
        "Snow-Covered Plains",
        "Snow-Covered Island",
        "Snow-Covered Swamp",
        "Snow-Covered Mountain",
        "Snow-Covered Forest",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
        momir_fixed_deck_names()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        snow_basics
    );
    let mut momir = GameState::new(FormatConfig::momir(), 2, 7);
    load_and_hydrate_decks(&mut momir, &DeckPayload::default(), Some(db));
    assert_eq!(momir.players[0].library.len(), 60);
    assert_eq!(momir.players[1].library.len(), 60);
    assert_eq!(momir.deck_pools.len(), 2);
    assert!(momir.seats_with_empty_library().is_empty());

    let mut standard = GameState::new_two_player(7);
    assert_eq!(
        standard.seats_with_empty_library(),
        vec![P0, P1],
        "an empty per-seat library is reported for each seat"
    );
    let id = engine::game::zones::create_object(
        &mut standard,
        engine::types::identifiers::CardId(1),
        P0,
        "Island".to_string(),
        Zone::Library,
    );
    assert_eq!(standard.seats_with_empty_library(), vec![P1]);
    assert_eq!(ids(standard.library_of(P0)), vec![id]);
    assert!(standard.library_of(P1).is_empty());
}

#[test]
fn v2_fixed_list_resolves_against_the_real_database() {
    let Some(db) = shared_card_db() else { return };
    let names = dandan_fixed_deck_names();
    assert_eq!(names.len(), 80);
    let distinct: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(distinct.len(), 23);
    assert_eq!(
        FormatConfig::dandan().deck_size,
        DeckSizeRule::Exactly(names.len() as u16)
    );
    for name in &distinct {
        assert!(
            db.get_face_by_name(name).is_some(),
            "{name} must resolve in the card database"
        );
    }
    assert_eq!(
        momir_fixed_deck_names().len(),
        60,
        "the Momir list is unchanged"
    );
}

#[test]
fn v10_pool_resolver_maps_both_seats_to_the_pile_holder() {
    let Some(db) = shared_card_db() else { return };
    let state = loaded_dandan(db, 7);
    assert_eq!(state.deck_pool_of(P1).map(|pool| pool.player), Some(P0));
    assert_eq!(state.deck_pool_of(P0).map(|pool| pool.player), Some(P0));

    let mut momir = GameState::new(FormatConfig::momir(), 2, 7);
    load_and_hydrate_decks(&mut momir, &DeckPayload::default(), Some(db));
    for seat in [P0, P1] {
        assert_eq!(momir.deck_pool_of(seat).map(|pool| pool.player), Some(seat));
    }
}

#[test]
fn v14_wasm_boot_guard_reads_the_library_accessor() {
    let start = WASM_LIB_RS
        .find("fn initialize_game_impl")
        .expect("initialize_game_impl exists");
    let end = start
        + WASM_LIB_RS[start..]
            .find("// CR 103.1: Start the game")
            .expect("the CR 103.1 marker follows the boot guard");
    let slice = &WASM_LIB_RS[start..end];
    let production = code_lines(slice);
    assert!(
        production.contains("Empty library after deck load"),
        "reach-guard"
    );
    assert!(production.contains("seats_with_empty_library("));
    assert!(!production.contains("library.is_empty()"));
}

// ---------------------------------------------------------------------------
// Scenario helpers
// ---------------------------------------------------------------------------

fn scenario(format: FormatConfig) -> GameScenario {
    let mut scenario = GameScenario::new_with_format(format, 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Stage real cards at the bottom of their owners' libraries, in order.
fn stage_library(
    scenario: &mut GameScenario,
    db: &CardDatabase,
    cards: &[(PlayerId, &str)],
) -> Vec<ObjectId> {
    cards
        .iter()
        .map(|&(owner, name)| scenario.add_real_card(owner, name, Zone::Library, db))
        .collect()
}

fn give_priority(runner: &mut GameRunner, seat: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = seat;
    state.waiting_for = WaitingFor::Priority { player: seat };
}

fn blue_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect()
}

/// Mixed-owner pile of real cards; P0 owns the even slots and P1 the odd ones.
const MIXED_PILE: [(PlayerId, &str); 6] = [
    (P0, "Island"),
    (P1, "Memory Lapse"),
    (P0, "Brainstorm"),
    (P1, "Island"),
    (P0, "Predict"),
    (P1, "Control Magic"),
];

// ---------------------------------------------------------------------------
// V3 / V4: opening deal and mulligan shuffle on the one pile
// ---------------------------------------------------------------------------

fn booted(db: &CardDatabase, starting: PlayerId) -> GameState {
    let mut state = loaded_dandan(db, 7);
    let _ = start_game_with_starting_player(&mut state, starting);
    state
}

#[test]
fn v4_opening_deal_draws_both_hands_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    let mut state = loaded_dandan(db, 7);
    let result = start_game_with_starting_player(&mut state, P1);

    assert_eq!(state.library_of(P0).len(), 80 - 14);
    assert!(state.players[1].library.is_empty());
    for seat in [P0, P1] {
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::CardsDrawn { player_id, count: 7 } if *player_id == seat
            )),
            "{seat:?} drew its opening hand"
        );
    }
}

#[test]
fn v3_mulligan_by_either_seat_shuffles_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (mulliganing, starting) in [(P1, P1), (P0, P0)] {
        let mut state = booted(db, starting);
        let before = ids(state.library_of(P0));
        let hand: Vec<ObjectId> = state.players[mulliganing.0 as usize]
            .hand
            .iter()
            .copied()
            .collect();
        // The pre-shuffle expectation: the hand goes to the bottom, seven are drawn from the top.
        let mut unshuffled = before.clone();
        unshuffled.extend(hand.iter().copied());
        let unshuffled: Vec<ObjectId> = unshuffled.into_iter().skip(7).collect();

        apply(
            &mut state,
            mulliganing,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Mulligan,
            },
        )
        .expect("mulligan accepted");
        let other = if mulliganing == P0 { P1 } else { P0 };
        apply(
            &mut state,
            other,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        )
        .expect("the other seat's keep closes the declare round");

        let after = ids(state.library_of(P0));
        assert_eq!(
            after.len(),
            before.len() + hand.len() - 7,
            "{mulliganing:?}"
        );
        assert!(state.players[1].library.is_empty());
        assert_ne!(
            after, unshuffled,
            "{mulliganing:?}: the mulligan shuffled the shared pile"
        );
    }
}

#[test]
fn shuffling_the_empty_seat_consumes_no_entropy() {
    let mut state = GameState::new_two_player(7);
    let before = state.rng.get_word_pos();
    engine::util::im_ext::shuffle_vector(&mut im::Vector::<ObjectId>::new(), &mut state.rng);
    assert_eq!(state.rng.get_word_pos(), before);

    let mut pile: im::Vector<ObjectId> = (1..=8).map(ObjectId).collect();
    engine::util::im_ext::shuffle_vector(&mut pile, &mut state.rng);
    assert_ne!(
        state.rng.get_word_pos(),
        before,
        "reach: a real shuffle does consume"
    );
}

// ---------------------------------------------------------------------------
// V5 / V6: draw selection and graveyard writes through the accessors
// ---------------------------------------------------------------------------

/// `cycler` holds a real Lonely Sandbar and {U} with priority; the library
/// is `MIXED_PILE`, topped by its first entries.
fn cycling_game(
    db: &CardDatabase,
    format: FormatConfig,
    cycler: PlayerId,
) -> (GameRunner, ObjectId) {
    let mut scenario = scenario(format);
    stage_library(&mut scenario, db, &MIXED_PILE);
    let sandbar = scenario.add_real_card(cycler, "Lonely Sandbar", Zone::Hand, db);
    scenario.with_mana_pool(cycler, blue_mana(1));
    let mut runner = scenario.build();
    give_priority(&mut runner, cycler);
    (runner, sandbar)
}

fn cycle(runner: &mut GameRunner, sandbar: ObjectId) {
    let index = runner.state().objects[&sandbar]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Cycling))
        .expect("Lonely Sandbar has a cycling ability");
    runner.activate(sandbar, index).resolve();
}

#[test]
fn v5_v6_either_seat_cycles_from_and_into_the_shared_zones() {
    let Some(db) = shared_card_db() else { return };
    for cycler in [P1, P0] {
        let (mut runner, sandbar) = cycling_game(db, FormatConfig::dandan(), cycler);
        let pile_before = ids(runner.state().library_of(cycler));
        assert_eq!(pile_before.len(), MIXED_PILE.len(), "reach: pile staged");
        assert!(runner.state().players[1].library.is_empty());

        cycle(&mut runner, sandbar);

        let state = runner.state();
        assert_eq!(
            ids(state.graveyard_of(cycler)),
            vec![sandbar],
            "{cycler:?}: the cycling cost was paid into the shared graveyard"
        );
        assert_eq!(ids(state.graveyard_of(P0)), ids(state.graveyard_of(P1)));
        assert!(state.players[1].graveyard.is_empty());
        assert_eq!(
            state.objects[&sandbar].owner, cycler,
            "ownership is untouched"
        );
        assert_eq!(
            ids(state.library_of(cycler)),
            pile_before[1..].to_vec(),
            "{cycler:?}: the draw took the top of the shared pile"
        );
        assert!(!state.players[cycler.0 as usize].drew_from_empty_library);
    }
}

#[test]
fn v5_v6_standard_format_cycles_in_the_owners_own_zones() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, sandbar) = cycling_game(db, FormatConfig::standard(), P1);
    let own_before = ids(&runner.state().players[1].library.clone());
    assert_eq!(own_before.len(), 3, "reach: P1's own library staged");

    cycle(&mut runner, sandbar);

    let state = runner.state();
    assert_eq!(ids(&state.players[1].graveyard), vec![sandbar]);
    assert_eq!(ids(&state.players[1].library), own_before[1..].to_vec());
}

// ---------------------------------------------------------------------------
// V7: conjure into a shared library
// ---------------------------------------------------------------------------

#[test]
fn v7_conjure_into_the_library_lands_in_the_pile_only() {
    let Some(db) = shared_card_db() else { return };
    for (format, shared) in [
        (FormatConfig::dandan(), true),
        (FormatConfig::standard(), false),
    ] {
        let mut scenario = scenario(format);
        let staged = stage_library(&mut scenario, db, &[(P1, "Island"); 10]);
        let security = scenario.add_real_card(P1, "Mine Security", Zone::Hand, db);
        scenario.with_mana_pool(
            P1,
            (0..2)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        give_priority(&mut runner, P1);
        runner.state_mut().active_player = P1;
        assert_eq!(
            runner.state().library_of(P1).len(),
            staged.len(),
            "reach: library staged"
        );

        runner.cast(security).resolve();

        let state = runner.state();
        let conjured: Vec<ObjectId> = state
            .objects
            .values()
            .filter(|object| object.name == "Flametongue Kavu")
            .map(|object| object.id)
            .collect();
        assert_eq!(
            conjured.len(),
            1,
            "the enter trigger conjured exactly one card"
        );
        let conjured = conjured[0];
        assert_eq!(state.objects[&conjured].zone, Zone::Library);
        assert_eq!(state.objects[&conjured].owner, P1);
        let library = ids(state.library_of(P1));
        assert_eq!(library.len(), staged.len() + 1);
        let slot = library
            .iter()
            .position(|id| *id == conjured)
            .expect("in the library");
        assert!(
            slot < 8,
            "conjured into the top eight at random, got slot {slot}"
        );
        assert_no_id_in_two_containers(state);
        assert_eq!(state.players[1].library.is_empty(), shared);
    }
}

// ---------------------------------------------------------------------------
// V8: recorded commands and journal replay
// ---------------------------------------------------------------------------

#[test]
fn v8_zone_change_commands_record_and_replay_pile_positions() {
    let Some(db) = shared_card_db() else { return };
    for (format, shared) in [
        (FormatConfig::dandan(), true),
        (FormatConfig::standard(), false),
    ] {
        let mut scenario = scenario(format);
        stage_library(&mut scenario, db, &MIXED_PILE);
        scenario.add_real_card(P0, "Island", Zone::Graveyard, db);
        scenario.add_real_card(P1, "Island", Zone::Graveyard, db);
        let to_library = scenario.add_real_card(P1, "Brainstorm", Zone::Hand, db);
        let to_graveyard = scenario.add_real_card(P1, "Predict", Zone::Hand, db);
        let mut state = scenario.build().state().clone();

        for (card, to) in [(to_library, Zone::Library), (to_graveyard, Zone::Graveyard)] {
            let pre = state.clone();
            let expected_position = match to {
                Zone::Library => pre.library_of(P1).len(),
                _ => pre.graveyard_of(P1).len(),
            };
            assert!(
                expected_position > 0,
                "reach: the target container is non-empty"
            );
            let record = state.objects[&card].snapshot_for_zone_change(card, Some(Zone::Hand), to);
            let command = zones::resolve_and_apply_zone_change(
                &mut state,
                card,
                Zone::Hand,
                to,
                P1,
                None,
                record,
            )
            .expect("zone change applies");

            assert_eq!(command.owner, P1);
            assert_eq!(
                command.destination_position, expected_position,
                "{to:?} shared={shared}"
            );
            let (live, replayed) = {
                let mut replay = pre;
                zones::apply_resolved_zone_change(&mut replay, &command).expect("replay applies");
                match to {
                    Zone::Library => (ids(state.library_of(P1)), ids(replay.library_of(P1))),
                    _ => (ids(state.graveyard_of(P1)), ids(replay.graveyard_of(P1))),
                }
            };
            assert_eq!(live, replayed);
            assert_eq!(live.last(), Some(&card), "appended to the container's end");
        }
    }
}

#[test]
fn v8_library_shuffle_acts_on_the_pile_and_replays() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = scenario(FormatConfig::dandan());
    stage_library(&mut scenario, db, &MIXED_PILE);
    let mut state = scenario.build().state().clone();
    let pre = state.clone();
    let before = ids(state.library_of(P0));

    let mut events = Vec::new();
    library::resolve_and_apply_library_shuffle(&mut state, P1, &mut events).expect("shuffle");

    let command = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::LibraryShuffle(command)) => Some(command.clone()),
            _ => None,
        })
        .expect("the shuffle is journaled");
    assert_eq!(command.player, P1, "the acting seat is recorded");
    assert_eq!(
        command.precondition_order, before,
        "the pile is the precondition"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::PlayerPerformedAction { player_id, action: PlayerActionKind::ShuffledLibrary, .. }
            if *player_id == P1
    )));
    let after = ids(state.library_of(P0));
    assert_ne!(after, before, "the pile order changed");
    assert!(state.players[1].library.is_empty());

    let mut replay = pre;
    library::apply_resolved_library_shuffle(&mut replay, &command, &mut Vec::new())
        .expect("replay applies");
    assert_eq!(ids(replay.library_of(P0)), after);
}

// ---------------------------------------------------------------------------
// V9: library knowledge keyed by the storage seat
// ---------------------------------------------------------------------------

/// P0 plays a real Halimar Depths over `MIXED_PILE` and stops at the
/// "look at the top three" choice, which records P0's knowledge of them.
fn halimar_dig(db: &CardDatabase, format: FormatConfig) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = scenario(format);
    stage_library(&mut scenario, db, &MIXED_PILE);
    let halimar = scenario.add_real_card(P0, "Halimar Depths", Zone::Hand, db);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&halimar].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: halimar,
            card_id,
        })
        .expect("land play accepted");
    runner.advance_until_stack_empty();
    let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!("expected DigChoice, got {:?}", runner.state().waiting_for);
    };
    (runner, cards)
}

#[test]
fn v9_library_boundary_by_either_seat_forgets_the_looked_at_pile() {
    let Some(db) = shared_card_db() else { return };
    for shuffler in [P1, P0] {
        let (mut runner, looked_at) = halimar_dig(db, FormatConfig::dandan());
        assert_eq!(looked_at.len(), 3);
        let owners: std::collections::BTreeSet<_> = looked_at
            .iter()
            .map(|id| runner.state().objects[id].owner)
            .collect();
        assert_eq!(
            owners.len(),
            2,
            "reach: the looked-at cards have both owners"
        );
        for id in &looked_at {
            assert!(
                runner.state().viewer_knows_card_identity(P0, *id),
                "reach: P0 knows {id:?} before the boundary"
            );
        }

        library::resolve_and_apply_library_shuffle(runner.state_mut(), shuffler, &mut Vec::new())
            .expect("shuffle");

        for id in &looked_at {
            assert!(
                !runner.state().viewer_knows_card_identity(P0, *id),
                "{shuffler:?}'s shuffle of the shared pile ended P0's knowledge of {id:?}"
            );
        }
    }
}

#[test]
fn v9_standard_shuffle_keeps_the_other_players_library_knowledge() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, looked_at) = halimar_dig(db, FormatConfig::standard());
    assert!(!looked_at.is_empty(), "reach: the dig looked at cards");
    for id in &looked_at {
        assert!(runner.state().viewer_knows_card_identity(P0, *id), "reach");
    }

    library::resolve_and_apply_library_shuffle(runner.state_mut(), P1, &mut Vec::new())
        .expect("shuffle");

    for id in &looked_at {
        assert!(
            runner.state().viewer_knows_card_identity(P0, *id),
            "P1's shuffle leaves P0's own library untouched"
        );
    }
}

// ---------------------------------------------------------------------------
// V15: in-library writers keyed through the storage seat
// ---------------------------------------------------------------------------

/// A Dandân game whose pile is `MIXED_PILE`, ready for a P1 library choice.
fn p1_choice_game(db: &CardDatabase) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = scenario(FormatConfig::dandan());
    let pile = stage_library(&mut scenario, db, &MIXED_PILE);
    let runner = scenario.build();
    assert!(
        runner.state().players[1].library.is_empty(),
        "reach: P1's own container is empty"
    );
    (runner, pile)
}

fn assert_pile_order(runner: &GameRunner, expected_top: &[ObjectId]) {
    let state = runner.state();
    let pile = ids(state.library_of(P1));
    assert_eq!(&pile[..expected_top.len()], expected_top);
    assert!(state.players[1].library.is_empty());
    assert_eq!(pile, ids(state.library_of(P0)));
    assert_no_id_in_two_containers(state);
}

#[test]
fn v15_dig_choice_reorders_the_pile_for_the_non_canonical_seat() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, pile) = p1_choice_game(db);
    let looked_at = pile[..3].to_vec();
    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P1,
        library_owner: P1,
        cards: looked_at.clone(),
        keep_count: 3,
        up_to: false,
        selectable_cards: looked_at.clone(),
        kept_destination: Some(Zone::Library),
        rest_destination: None,
        rest_split_top_count: None,
        rest_order: Default::default(),
        source_id: None,
        enter_tapped: false,
        enters_attacking: false,
    };
    let chosen = vec![looked_at[2], looked_at[0], looked_at[1]];

    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("dig choice accepted");

    assert_pile_order(&runner, &chosen);
}

#[test]
fn v15_scry_choice_reorders_the_pile_for_the_non_canonical_seat() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, pile) = p1_choice_game(db);
    let looked_at = pile[..2].to_vec();
    runner.state_mut().waiting_for = WaitingFor::ScryChoice {
        player: P1,
        cards: looked_at.clone(),
    };

    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[1]],
        })
        .expect("scry choice accepted");

    let state = runner.state();
    let reordered = ids(state.library_of(P1));
    assert_eq!(reordered[0], looked_at[1], "kept on top");
    assert_eq!(
        *reordered.last().unwrap(),
        looked_at[0],
        "the other went to the bottom"
    );
    assert!(state.players[1].library.is_empty());
    assert_no_id_in_two_containers(state);
}

#[test]
fn v15_library_placement_by_the_zone_pipeline_targets_the_pile() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, _pile) = p1_choice_game(db);
    let card = zones::create_object(
        runner.state_mut(),
        CardId(900),
        P1,
        "Brainstorm".to_string(),
        Zone::Hand,
    );
    zones::move_to_library_position(runner.state_mut(), card, true, &mut Vec::new());

    assert_eq!(runner.state().objects[&card].zone, Zone::Library);
    assert_pile_order(&runner, &[card]);
}

#[test]
fn v15_surveil_keeps_cards_on_top_of_the_pile_for_the_non_canonical_seat() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, pile) = p1_choice_game(db);
    let looked_at = pile[..2].to_vec();
    runner.state_mut().waiting_for = WaitingFor::SurveilChoice {
        player: P1,
        cards: looked_at.clone(),
    };
    let kept = vec![looked_at[1], looked_at[0]];

    runner
        .act(GameAction::SelectCards {
            cards: kept.clone(),
        })
        .expect("surveil choice accepted");

    assert_pile_order(&runner, &kept);
}

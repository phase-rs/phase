//! Dandân hand-entry ownership: a card that enters a hand from the shared
//! library or graveyard is owned by the player whose hand receives it, so every
//! later "owner's hand", cast and land-play reads the receiver.

use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{load_and_hydrate_decks, DeckPayload};
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityTag;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::dandan_filter_owner_axis::{dandan, plenty_of_mana, scenario, stage, start};
use crate::support::shared_card_db;

fn pile(state: &GameState) -> Vec<ObjectId> {
    state.library_of(P1).iter().copied().collect()
}

fn hand_of(state: &GameState, seat: PlayerId) -> Vec<ObjectId> {
    state.players[seat.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

/// `id` sits in `seat`'s hand only, is owned and controlled by `seat`, and its
/// arrival record names `seat` as owner.
fn assert_hand(state: &GameState, seat: PlayerId, id: ObjectId, label: &str) {
    let object = &state.objects[&id];
    for player in &state.players {
        assert_eq!(
            player.hand.contains(&id),
            player.id == seat,
            "{label}: {id:?} is in exactly {seat:?}'s hand (checked {:?})",
            player.id
        );
    }
    assert_eq!(object.zone, Zone::Hand, "{label}");
    assert_eq!(object.owner, seat, "{label}: owner");
    assert_eq!(object.controller, seat, "{label}: controller");
    let record = state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|record| record.object_id == id && record.to_zone == Zone::Hand)
        .unwrap_or_else(|| panic!("{label}: a Hand arrival record exists"));
    assert_eq!(record.arrival.owner, seat, "{label}: arrival record owner");
}

fn give_turn(runner: &mut GameRunner, seat: PlayerId) {
    let state = runner.state_mut();
    state.active_player = seat;
    state.priority_player = seat;
    state.waiting_for = WaitingFor::Priority { player: seat };
}

fn ability_index(
    runner: &GameRunner,
    source: ObjectId,
    tag: Option<AbilityTag>,
    cost: &str,
) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| match tag {
            Some(tag) => ability.ability_tag == Some(tag),
            None => format!("{:?}", ability.cost).contains(cost),
        })
        .expect("the ability exists")
}

/// Pass priority and answer targeting prompts until one of the resolution
/// prompts a row asserts on parks, or the stack is empty.
fn run_to_prompt(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner.choose_first_legal_target().expect("target chosen");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("never settled: {:?}", runner.state().waiting_for);
}

fn select(runner: &mut GameRunner, cards: Vec<ObjectId>) {
    runner
        .act(GameAction::SelectCards { cards })
        .expect("selection accepted");
}

/// Pile staged top first: a P0-owned Dandân, Island and Lonely Sandbar.
const TOP_THREE: [(PlayerId, &str); 3] = [(P0, "Dandân"), (P0, "Island"), (P0, "Lonely Sandbar")];

/// `actor` holds a real Brainstorm over `TOP_THREE` (owned by `owner`), atop a
/// longer pile.
fn brainstorm_game(
    db: &CardDatabase,
    format: FormatConfig,
    actor: PlayerId,
    owner: PlayerId,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut sc = scenario(format);
    let cards: Vec<(PlayerId, &str)> = TOP_THREE
        .iter()
        .map(|&(_, name)| (owner, name))
        .chain([(owner, "Island"), (owner, "Island")])
        .collect();
    let staged = stage(&mut sc, db, Zone::Library, &cards);
    let spell = sc.add_real_card(actor, "Brainstorm", Zone::Hand, db);
    (start(sc, actor), spell, staged)
}

// ---------------------------------------------------------------------------
// V1: draw
// ---------------------------------------------------------------------------

#[test]
fn v1_brainstorm_draw_from_the_pile_is_owned_by_the_drawer() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, staged) = brainstorm_game(db, dandan(), P1, P0);
    let top = staged[..3].to_vec();
    assert_eq!(pile(runner.state())[..3], top[..], "reach: pile staged");

    runner.cast(spell).effect_zone(&[top[1], top[2]]).resolve();

    let state = runner.state();
    assert_hand(state, P1, top[0], "the card kept in hand");
    for id in &top[1..] {
        assert_eq!(
            state.objects[id].owner, P1,
            "put back, still owned by the drawer"
        );
        let record = state
            .zone_changes_this_turn
            .iter()
            .find(|record| record.object_id == *id && record.to_zone == Zone::Hand)
            .expect("the draw arrival is recorded");
        assert_eq!(
            record.arrival.owner, P1,
            "the draw arrival record names the drawer"
        );
    }
    assert_eq!(
        pile(state)[..2],
        [top[1], top[2]],
        "the two put back are on top"
    );
    assert!(hand_of(state, P0).is_empty());
}

#[test]
fn v1_brainstorm_from_the_pile_holder_and_in_standard_are_unchanged() {
    let Some(db) = shared_card_db() else { return };
    for (format, actor, owner) in [(dandan(), P0, P0), (FormatConfig::standard(), P1, P1)] {
        let (mut runner, spell, staged) = brainstorm_game(db, format, actor, owner);
        runner
            .cast(spell)
            .effect_zone(&[staged[1], staged[2]])
            .resolve();
        assert_hand(runner.state(), actor, staged[0], "the drawer's own card");
    }
}

// ---------------------------------------------------------------------------
// V2: The Surgical Bay (draw, play, sacrifice-draw)
// ---------------------------------------------------------------------------

#[test]
fn v2_a_drawn_land_is_played_by_the_drawer_and_sacrificed_into_the_pile() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let staged = stage(
        &mut sc,
        db,
        Zone::Library,
        &[(P0, "The Surgical Bay"), (P0, "Island"), (P0, "Dandân")],
    );
    let sandbar = sc.add_real_card(P1, "Lonely Sandbar", Zone::Hand, db);
    let mut runner = start(sc, P1);

    let cycling = ability_index(&runner, sandbar, Some(AbilityTag::Cycling), "");
    runner.activate(sandbar, cycling).resolve();
    let bay = staged[0];
    assert_hand(runner.state(), P1, bay, "the cycled-into land");

    let card_id = runner.state().objects[&bay].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: bay,
            card_id,
        })
        .expect("the receiver may play the card: it is in their hand");
    runner.state_mut().objects.get_mut(&bay).unwrap().tapped = false;
    let sacrifice = ability_index(&runner, bay, None, "Sacrifice");
    runner.activate(bay, sacrifice).resolve();

    let state = runner.state();
    assert_eq!(state.objects[&bay].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&bay].owner, P1,
        "the Bay left as the drawer's card"
    );
    assert_hand(state, P1, staged[1], "the sacrifice draw");
}

// ---------------------------------------------------------------------------
// V3: opening deal and mulligan redraw
// ---------------------------------------------------------------------------

fn booted(db: &CardDatabase) -> GameState {
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
    let _ = start_game_with_starting_player(&mut state, P1);
    state
}

#[test]
fn v3_opening_deal_and_mulligan_redraw_belong_to_the_receiving_seat() {
    let Some(db) = shared_card_db() else { return };
    let mut state = booted(db);
    for seat in [P0, P1] {
        let hand = hand_of(&state, seat);
        assert_eq!(hand.len(), 7, "{seat:?}: reach: a seven-card hand");
        for id in hand {
            assert_eq!(state.objects[&id].owner, seat, "{seat:?}: dealt card owner");
        }
    }
    let p0_before = hand_of(&state, P0);

    apply(
        &mut state,
        P1,
        GameAction::MulliganDecision {
            choice: MulliganChoice::Mulligan,
        },
    )
    .expect("mulligan accepted");
    apply(
        &mut state,
        P0,
        GameAction::MulliganDecision {
            choice: MulliganChoice::Keep,
        },
    )
    .expect("P0's keep closes the declare round");

    let hand = hand_of(&state, P1);
    assert_eq!(hand.len(), 7, "reach: P1 redrew seven");
    for id in hand {
        assert_eq!(state.objects[&id].owner, P1, "redrawn card owner");
    }
    assert_eq!(hand_of(&state, P0), p0_before, "P0's hand is untouched");
}

// ---------------------------------------------------------------------------
// V4 / V4b: Dig to hand and to exile
// ---------------------------------------------------------------------------

fn telling_time_game(db: &CardDatabase, actor: PlayerId) -> (GameRunner, Vec<ObjectId>) {
    let mut sc = scenario(dandan());
    let mut staged = stage(&mut sc, db, Zone::Library, &TOP_THREE);
    staged.extend(stage(&mut sc, db, Zone::Library, &[(P0, "Island")]));
    let spell = sc.add_real_card(actor, "Telling Time", Zone::Hand, db);
    let mut runner = start(sc, actor);
    runner.cast(spell).commit();
    run_to_prompt(&mut runner);
    (runner, staged)
}

#[test]
fn v4_telling_time_puts_the_kept_card_into_the_receivers_hand() {
    let Some(db) = shared_card_db() else { return };
    for actor in [P1, P0] {
        let (mut runner, staged) = telling_time_game(db, actor);
        let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
            panic!("expected DigChoice, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(cards, staged[..3].to_vec(), "reach: the pile's top three");

        select(&mut runner, vec![staged[0]]);
        let WaitingFor::DigRestSplitChoice { cards: rest, .. } = runner.state().waiting_for.clone()
        else {
            panic!("expected DigRestSplitChoice");
        };
        select(&mut runner, rest.clone());

        assert_hand(runner.state(), actor, staged[0], &format!("{actor:?} kept"));
        for id in &rest {
            assert_eq!(runner.state().objects[id].zone, Zone::Library);
        }
    }
}

#[test]
fn v4b_dig_to_exile_does_not_name_a_performer() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let staged = stage(
        &mut sc,
        db,
        Zone::Library,
        &[
            (P0, "Dandân"),
            (P0, "Island"),
            (P0, "Island"),
            (P0, "Island"),
            (P0, "Island"),
        ],
    );
    let gonti = sc.add_real_card(P1, "Gonti, Lord of Luxury", Zone::Hand, db);
    let mut runner = start(sc, P1);
    runner.cast(gonti).commit();
    run_to_prompt(&mut runner);
    let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!("expected DigChoice, got {:?}", runner.state().waiting_for);
    };
    assert_eq!(cards, staged[..4].to_vec(), "reach: the pile's top four");

    select(&mut runner, vec![staged[0]]);

    let object = &runner.state().objects[&staged[0]];
    assert_eq!(object.zone, Zone::Exile, "reach: kept to exile");
    assert_eq!(object.owner, P0, "a move to exile never rebinds");
    assert_eq!(object.exiled_by, None, "the exile record is the base value");
}

#[test]
fn v4c_dig_put_all_into_hand_belongs_to_the_receiver() {
    let Some(db) = shared_card_db() else { return };
    for (format, actor, owner) in [
        (dandan(), P1, P0),
        (dandan(), P0, P0),
        (FormatConfig::standard(), P1, P1),
    ] {
        let mut sc = scenario(format);
        let mut cards = vec![(owner, "Control Magic")];
        cards.extend([(owner, "Island"); 6]);
        let staged = stage(&mut sc, db, Zone::Library, &cards);
        let marina = sc.add_real_card(actor, "Marina Vendrell", Zone::Hand, db);
        let mut runner = start(sc, actor);
        assert_eq!(
            runner.state().objects[&staged[0]].zone,
            Zone::Library,
            "reach: the enchantment starts in the library"
        );

        runner.cast(marina).commit();
        run_to_prompt(&mut runner);

        assert_hand(
            runner.state(),
            actor,
            staged[0],
            &format!("{actor:?} Marina over {owner:?}'s cards"),
        );
    }
}

// ---------------------------------------------------------------------------
// V5: ChangeZone graveyard to hand
// ---------------------------------------------------------------------------

#[test]
fn v5_haunted_fengraf_returns_the_pile_card_to_the_activators_ownership() {
    let Some(db) = shared_card_db() else { return };
    for actor in [P1, P0] {
        let mut sc = scenario(dandan());
        let fengraf = sc.add_real_card(actor, "Haunted Fengraf", Zone::Battlefield, db);
        let victim = sc.add_real_card(P0, "Dandân", Zone::Graveyard, db);
        let mut runner = start(sc, actor);
        runner.state_mut().objects.get_mut(&fengraf).unwrap().tapped = false;
        let index = ability_index(&runner, fengraf, None, "Sacrifice");

        runner.activate(fengraf, index).resolve();

        let state = runner.state();
        assert_eq!(
            state.objects[&fengraf].zone,
            Zone::Graveyard,
            "reach: cost paid"
        );
        assert_hand(state, actor, victim, &format!("{actor:?} Fengraf"));
    }
}

// ---------------------------------------------------------------------------
// V6: draw, cast, steal, bounce
// ---------------------------------------------------------------------------

#[test]
fn v6_a_stolen_drawn_creature_returns_to_the_drawers_hand() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let staged = stage(
        &mut sc,
        db,
        Zone::Library,
        &[(P0, "Dandân"), (P0, "Island")],
    );
    let sandbar = sc.add_real_card(P1, "Lonely Sandbar", Zone::Hand, db);
    let bounce = sc.add_real_card(P1, "Unsubstantiate", Zone::Hand, db);
    let steal = sc.add_real_card(P0, "Control Magic", Zone::Hand, db);
    sc.add_real_card(P1, "Island", Zone::Battlefield, db);
    sc.add_real_card(P0, "Island", Zone::Battlefield, db);
    sc.with_mana_pool(P0, plenty_of_mana());
    let mut runner = start(sc, P1);
    let dandan_card = staged[0];

    let cycling = ability_index(&runner, sandbar, Some(AbilityTag::Cycling), "");
    runner.activate(sandbar, cycling).resolve();
    assert_hand(runner.state(), P1, dandan_card, "drawn");

    runner.cast(dandan_card).resolve();
    assert_eq!(runner.state().objects[&dandan_card].zone, Zone::Battlefield);

    give_turn(&mut runner, P0);
    runner.cast(steal).target_object(dandan_card).resolve();
    let object = &runner.state().objects[&dandan_card];
    assert_eq!(object.controller, P0, "reach: Control Magic took control");
    assert_eq!(object.owner, P1, "the thief controls, the drawer owns");

    give_turn(&mut runner, P1);
    runner.cast(bounce).target_object(dandan_card).resolve();
    assert_hand(runner.state(), P1, dandan_card, "bounced to its owner");
    assert!(hand_of(runner.state(), P0)
        .iter()
        .all(|id| *id != dandan_card));
}

// ---------------------------------------------------------------------------
// V7: census siblings
// ---------------------------------------------------------------------------

#[test]
fn v7_explore_puts_a_revealed_land_into_the_explorers_hand() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let staged = stage(
        &mut sc,
        db,
        Zone::Library,
        &[(P0, "Island"), (P0, "Island"), (P0, "Dandân")],
    );
    let ranger = sc.add_real_card(P1, "Jadelight Ranger", Zone::Hand, db);
    let mut runner = start(sc, P1);
    runner.cast(ranger).commit();
    run_to_prompt(&mut runner);

    assert_hand(runner.state(), P1, staged[0], "first explore");
    assert_hand(runner.state(), P1, staged[1], "second explore");
}

#[test]
fn v7_seek_puts_the_found_card_into_the_seekers_hand() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let staged = stage(
        &mut sc,
        db,
        Zone::Library,
        &[(P0, "Dandân"), (P0, "Island")],
    );
    let wrangler = sc.add_real_card(P1, "Hollowhenge Wrangler", Zone::Hand, db);
    let mut runner = start(sc, P1);
    runner.cast(wrangler).commit();
    run_to_prompt(&mut runner);

    assert_hand(runner.state(), P1, staged[1], "the sought land");
}

// ---------------------------------------------------------------------------
// V8: every hand card is owned by its holder
// ---------------------------------------------------------------------------

#[test]
fn v8_every_card_in_a_hand_is_owned_by_its_holder_after_mixed_flows() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let staged = stage(
        &mut sc,
        db,
        Zone::Library,
        &[
            (P0, "Dandân"),
            (P0, "Island"),
            (P0, "Lonely Sandbar"),
            (P0, "Island"),
            (P0, "Island"),
        ],
    );
    let spell = sc.add_real_card(P1, "Brainstorm", Zone::Hand, db);
    let fengraf = sc.add_real_card(P1, "Haunted Fengraf", Zone::Battlefield, db);
    let victim = sc.add_real_card(P0, "Dandân", Zone::Graveyard, db);
    sc.add_real_card(P0, "Island", Zone::Hand, db);
    let mut runner = start(sc, P1);
    runner.state_mut().objects.get_mut(&fengraf).unwrap().tapped = false;

    runner
        .cast(spell)
        .effect_zone(&[staged[1], staged[2]])
        .resolve();
    let index = ability_index(&runner, fengraf, None, "Sacrifice");
    runner.activate(fengraf, index).resolve();

    let state = runner.state();
    assert!(
        hand_of(state, P1).contains(&victim) && hand_of(state, P1).contains(&staged[0]),
        "reach: P1 holds cards the pile staged under P0"
    );
    for seat in [P0, P1] {
        for id in hand_of(state, seat) {
            assert_eq!(state.objects[&id].owner, seat, "{seat:?} holds {id:?}");
        }
    }
}

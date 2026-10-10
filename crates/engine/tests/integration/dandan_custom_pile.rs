//! Dandân host-supplied pile: the pile seat's submitted 80 becomes the one
//! shared library on every load path, an empty submission plays the default
//! list, and the validator admits any resolvable 80 with no ban list and no
//! copy limit (CR 407.3's ante refusal stays).

use std::collections::BTreeMap;
use std::io::BufReader;
use std::sync::Arc;

use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
};
use engine::game::deck_validation::{
    evaluate_deck_format_gate, validate_name_deck_for_format_full, DeckCompatibilityRequest,
};
use engine::game::engine::start_game_with_starting_player;
use engine::game::replay::reconstruct_initial_state;
use engine::game::scenario::{P0, P1};
use engine::types::format::{FormatConfig, GameFormat, SelectedFormat};
use engine::types::game_state::GameState;
use engine::types::match_config::MatchConfig;
use engine::types::replay::ReplayHeader;
use flate2::read::GzDecoder;

use crate::dandan_shared_pile_storage::expected_decklist;
use crate::support::shared_card_db;

const JUND_PILE: [(&str, usize); 6] = [
    ("Forest", 14),
    ("Swamp", 14),
    ("Mountain", 12),
    ("Lightning Bolt", 16),
    ("Bloodbraid Elf", 12),
    ("Hymn to Tourach", 12),
];

fn names(list: &[(&str, usize)]) -> Vec<String> {
    list.iter()
        .flat_map(|&(name, copies)| std::iter::repeat_n(name.to_string(), copies))
        .collect()
}

fn multiset(names: impl IntoIterator<Item = String>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for name in names {
        *counts.entry(name).or_insert(0) += 1;
    }
    counts
}

fn jund_multiset() -> BTreeMap<String, usize> {
    multiset(names(&JUND_PILE))
}

/// The Jund pile on seat 0 and, unless `None`, `opponent` on seat 1.
fn jund_list(opponent: Option<Vec<String>>) -> DeckList {
    DeckList {
        player: PlayerDeckList {
            main_deck: names(&JUND_PILE),
            ..Default::default()
        },
        opponent: PlayerDeckList {
            main_deck: opponent.unwrap_or_default(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn loaded(db: &CardDatabase, payload: &DeckPayload) -> GameState {
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    load_and_hydrate_decks(&mut state, payload, Some(db));
    state
}

fn library_names(state: &GameState) -> Vec<String> {
    state
        .library_of(P0)
        .iter()
        .map(|id| state.objects[id].name.clone())
        .collect()
}

fn object_names(state: &GameState) -> BTreeMap<String, usize> {
    multiset(state.objects.values().map(|object| object.name.clone()))
}

#[test]
fn the_pile_seats_submission_is_the_one_shared_library() {
    let Some(db) = shared_card_db() else { return };
    let mut payload = resolve_deck_list(db, &jund_list(Some(vec!["Island".to_string(); 80])));
    // A card split across two entries still loads as one card's copies.
    let forest = payload
        .player
        .main_deck
        .iter()
        .position(|entry| entry.card.name == "Forest")
        .expect("Forest resolves");
    payload.player.main_deck[forest].count = 7;
    let split = payload.player.main_deck[forest].clone();
    payload.player.main_deck.push(split);

    let state = loaded(db, &payload);

    assert_eq!(state.library_of(P0).len(), 80);
    assert!(state.players[1].library.is_empty());
    assert_eq!(multiset(library_names(&state)), jund_multiset());
    assert_eq!(state.deck_pools.len(), 1);
    assert_eq!(state.deck_pool_of(P1).map(|pool| pool.player), Some(P0));
    let pool = multiset(
        state.deck_pools[0]
            .current_main
            .iter()
            .flat_map(|entry| std::iter::repeat_n(entry.card.name.clone(), entry.count as usize)),
    );
    assert_eq!(pool, jund_multiset());
}

#[test]
fn an_empty_submission_plays_the_default_list() {
    let Some(db) = shared_card_db() else { return };
    let state = loaded(db, &resolve_deck_list(db, &DeckList::default()));
    assert_eq!(multiset(library_names(&state)), expected_decklist());
}

#[test]
fn only_the_pile_seats_main_deck_loads() {
    let Some(db) = shared_card_db() else { return };
    let mut list = jund_list(None);
    list.player.sideboard = vec!["Black Lotus".to_string()];
    list.player.commander = vec!["Amulet of Quoz".to_string()];
    let payload = resolve_deck_list(db, &list);
    assert_eq!(
        payload.player.sideboard.len(),
        1,
        "reach: sideboard submitted"
    );
    assert_eq!(
        payload.player.commander.len(),
        1,
        "reach: commander submitted"
    );

    let state = loaded(db, &payload);

    assert_eq!(multiset(library_names(&state)), jund_multiset());
    assert!(state.command_zone.is_empty(), "{:?}", state.command_zone);
    assert!(state.deck_pools[0].current_sideboard.is_empty());
    assert!(state.deck_pools[0].current_commander.is_empty());
    assert_eq!(object_names(&state), jund_multiset());
}

#[test]
fn a_started_game_plays_only_the_pile() {
    let Some(db) = shared_card_db() else { return };
    let mut state = loaded(db, &resolve_deck_list(db, &jund_list(None)));
    let _ = start_game_with_starting_player(&mut state, P0);

    assert_eq!(
        state.library_of(P0).len(),
        80 - 14,
        "reach: both hands dealt"
    );
    let hands = state.players.iter().map(|p| p.hand.len()).sum::<usize>();
    assert_eq!(hands, 14);
    assert_eq!(object_names(&state), jund_multiset());
}

fn owned_db() -> Arc<CardDatabase> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/integration_cards.json.gz");
    let file = std::fs::File::open(path).expect("fixture opens");
    Arc::new(
        CardDatabase::from_export_reader(GzDecoder::new(BufReader::new(file)))
            .expect("fixture loads"),
    )
}

fn header(deck_data: DeckList) -> ReplayHeader {
    ReplayHeader {
        format_config: FormatConfig::dandan(),
        match_config: MatchConfig::default(),
        player_count: 2,
        first_player: Some(0),
        seed: 11,
        deck_data: Some(deck_data),
    }
}

#[test]
fn a_replay_reloads_the_pile_from_the_recorded_deck_list() {
    let db = owned_db();
    let header = header(jund_list(None));

    let replayed = reconstruct_initial_state(&header, Some(&db)).expect("replay reconstructs");
    assert_eq!(object_names(&replayed), jund_multiset());

    // Preservation: replay and the live load agree for the same header and seed.
    let mut live = GameState::new(header.format_config.clone(), 2, header.seed);
    live.set_match_config(header.match_config);
    load_and_hydrate_decks(
        &mut live,
        &resolve_deck_list(&db, header.deck_data.as_ref().unwrap()),
        Some(&db),
    );
    let _ = start_game_with_starting_player(&mut live, P0);
    assert!(
        !library_names(&replayed).is_empty(),
        "reach: library non-empty"
    );
    assert_eq!(library_names(&replayed), library_names(&live));
}

fn validate(
    format: FormatConfig,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
) -> Result<(), Vec<String>> {
    let db = shared_card_db().expect("fixture");
    validate_name_deck_for_format_full(
        db,
        main_deck,
        sideboard,
        commander,
        &[],
        &[],
        &[],
        &[],
        &[],
        &format,
        None,
        2,
    )
}

fn dandan(main_deck: &[String]) -> Result<(), Vec<String>> {
    validate(FormatConfig::dandan(), main_deck, &[], &[])
}

fn assert_refused(result: Result<(), Vec<String>>, needle: &str) {
    let reasons = result.expect_err(needle);
    assert!(
        reasons.iter().any(|reason| reason.contains(needle)),
        "expected {needle:?} in {reasons:?}"
    );
}

/// `base` with its last `n` cards replaced by `card`.
fn with_tail(base: &[String], card: &str, n: usize) -> Vec<String> {
    let mut deck = base[..base.len() - n].to_vec();
    deck.extend(std::iter::repeat_n(card.to_string(), n));
    deck
}

#[test]
fn the_validator_admits_any_resolvable_80_and_an_empty_submission() {
    if shared_card_db().is_none() {
        return;
    }
    let jund = names(&JUND_PILE);
    assert_eq!(dandan(&[]), Ok(()));
    assert_eq!(dandan(&jund), Ok(()));
    assert_eq!(
        dandan(&vec!["Lightning Bolt".to_string(); 80]),
        Ok(()),
        "no copy limit"
    );
    assert_eq!(
        dandan(&with_tail(&jund, "Black Lotus", 4)),
        Ok(()),
        "no ban list"
    );

    assert_refused(dandan(&jund[1..]), "exactly 80 cards (found 79)");
    let mut eighty_one = jund.clone();
    eighty_one.push("Forest".to_string());
    assert_refused(dandan(&eighty_one), "exactly 80 cards (found 81)");
    assert_refused(
        dandan(&with_tail(&jund, "Not A Real Card Probe", 1)),
        "Not A Real Card Probe",
    );
    assert_refused(
        validate(FormatConfig::dandan(), &jund, &["Forest".to_string()], &[]),
        "sideboard",
    );
    assert_refused(
        validate(
            FormatConfig::dandan(),
            &jund,
            &[],
            &["Bloodbraid Elf".to_string()],
        ),
        "commander",
    );
    // CR 407.3: the ante card is refused where Black Lotus above passes.
    assert_refused(dandan(&with_tail(&jund, "Amulet of Quoz", 1)), "ante");
}

#[test]
fn the_p2p_guest_gate_admits_an_empty_dandan_submission() {
    let Some(db) = shared_card_db() else { return };
    let request = |format: GameFormat, main_deck: Vec<String>| DeckCompatibilityRequest {
        main_deck,
        selected_format: Some(SelectedFormat::Tag(format)),
        player_count: 2,
        ..Default::default()
    };
    assert!(evaluate_deck_format_gate(db, &request(GameFormat::Dandan, Vec::new())).compatible);
    assert!(
        evaluate_deck_format_gate(db, &request(GameFormat::Dandan, names(&JUND_PILE))).compatible
    );
    let short =
        evaluate_deck_format_gate(db, &request(GameFormat::Dandan, vec!["Forest".into(); 79]));
    assert!(
        !short.compatible,
        "reach: the gate still refuses a short pile"
    );
}

#[test]
fn the_empty_rule_follows_the_deck_supply_axis() {
    if shared_card_db().is_none() {
        return;
    }
    assert_refused(validate(FormatConfig::standard(), &[], &[], &[]), "found 0");
    assert_eq!(validate(FormatConfig::momir(), &[], &[], &[]), Ok(()));
    assert!(validate(FormatConfig::momir(), &names(&JUND_PILE), &[], &[]).is_err());
}

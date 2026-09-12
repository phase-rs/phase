//! Booster Tutor: "Open a sealed Magic booster pack, reveal the cards, and put
//! one of them into your hand."
//!
//! CR 400.11 + CR 400.11b + CR 701.20. Drives the real cast pipeline: the spell
//! resolves into an outside-the-game choice listing the whole opened pack, the
//! controller takes one card, that card becomes a real card in their hand, and
//! every other card in the pack goes NOWHERE — not exile, not a graveyard. They
//! were never in a zone (CR 400.11: "Outside the game is not a zone").
//!
//! Ordinary-pack controls install a synthetic product. Cube regressions load
//! the persisted source through DeckList and database hydration before casting.

use engine::database::CardDatabase;
use engine::game::boosters;
use engine::game::card_subset::{game_requires_full_card_db, FullDbReason};
use engine::game::deck_loading::{load_and_hydrate_decks, resolve_deck_list, DeckList};
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{GameAction, OutsideGameSelection};
use engine::types::card::{CardFace, Rarity};
use engine::types::card_type::{CardType, CoreType};
use engine::types::custom_format::{swedish_old_school, AntePolicy};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    BoosterProduct, BoosterShelf, GameState, OutsideGameChoiceSource, PackOrigin, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::match_config::{DeckCardCount, MatchType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::RngCore;
use std::sync::Arc;

/// Verbatim MTGJSON Oracle text, including its reminder.
const BOOSTER_TUTOR_ORACLE: &str =
    "Open a sealed Magic booster pack, reveal the cards, and put one of them into your hand. (Remove that card from your deck before beginning a new game.)";

const PACK_SET: &str = "TST";

fn face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            core_types: vec![CoreType::Creature],
            ..Default::default()
        },
        ..Default::default()
    }
}

/// One product with enough distinct cards to fill every slot of a pack, so the
/// collated pack is exactly the full skeleton (10 commons + 3 uncommons + 1
/// rare) with no short deal.
fn test_shelf() -> BoosterShelf {
    BoosterShelf::Products(vec![BoosterProduct {
        set_code: PACK_SET.to_string(),
        commons: (0..20).map(|i| face(&format!("Test Common {i}"))).collect(),
        uncommons: (0..8)
            .map(|i| face(&format!("Test Uncommon {i}")))
            .collect(),
        rares: (0..4).map(|i| face(&format!("Test Rare {i}"))).collect(),
        mythics: Vec::new(),
    }])
}

fn black_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![]); count]
}

/// CR 407.3 identifies the ante class by this printed clause, so the fixture
/// carries the real text rather than relying on the card's name.
const ANTE_CLAUSE: &str =
    "Remove this card from your deck before playing if you're not playing for ante.";

const ANTE_CARD: &str = "Jeweled Bird";

fn ante_face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            core_types: vec![CoreType::Artifact],
            ..Default::default()
        },
        oracle_text: Some(ANTE_CLAUSE.to_string()),
        ..Default::default()
    }
}

/// The same product as [`test_shelf`], except the rare slot's ONLY candidate is
/// an ante card. With one rare for one rare slot, the collated pack contains it
/// deterministically — the commons and uncommons stay ordinary, so every
/// assertion below has a live non-ante control in the same pack.
fn ante_shelf() -> BoosterShelf {
    BoosterShelf::Products(vec![BoosterProduct {
        set_code: PACK_SET.to_string(),
        commons: (0..20).map(|i| face(&format!("Test Common {i}"))).collect(),
        uncommons: (0..8)
            .map(|i| face(&format!("Test Uncommon {i}")))
            .collect(),
        rares: vec![ante_face(ANTE_CARD)],
        mythics: Vec::new(),
    }])
}

/// Opens a pack containing one ante card under a custom format declaring
/// `ante`, and returns the offered choices.
fn open_ante_pack_under(
    ante: AntePolicy,
) -> Vec<engine::types::game_state::OutsideGameChoiceEntry> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(1));
    let mut runner = scenario.build();
    runner.state_mut().booster_shelf = Arc::new(ante_shelf());

    // `for_custom_rules` is total and applies no gate of its own, which is what
    // lets this reach `AntePolicy::Enabled` — a value `passes_legacy_axis_gate`
    // refuses on every production path today.
    let mut rules = swedish_old_school().rules;
    rules.legality.legacy.ante = ante;
    runner.state_mut().format_config = FormatConfig::for_custom_rules(&rules);

    let outcome = runner.cast(tutor).resolve();
    let WaitingFor::OutsideGameChoice { choices, .. } = outcome.state().waiting_for.clone() else {
        panic!(
            "opening a pack must raise an outside-the-game choice, got {:?}",
            outcome.state().waiting_for
        );
    };
    choices
}

fn choice_names(choices: &[engine::types::game_state::OutsideGameChoiceEntry]) -> Vec<String> {
    choices.iter().map(|choice| choice.name.clone()).collect()
}

/// CR 407.3: "these cards can't be brought into the game from outside the
/// game." A booster pack is drawn from a set's whole card pool, so nothing the
/// deck-construction rules vetted stands between it and the battlefield — this
/// is the clause a banned/restricted list could never have covered.
#[test]
fn booster_pack_does_not_offer_an_ante_card_while_not_playing_for_ante() {
    let choices = open_ante_pack_under(AntePolicy::Excluded);
    let names = choice_names(&choices);

    assert!(
        !names.iter().any(|name| name == ANTE_CARD),
        "an ante card must not be offered from a pack (CR 407.3), got {names:?}"
    );
    // Discriminating on both sides: the rest of the pack is still selectable,
    // so this is the ante class being excluded and not the choice collapsing.
    assert_eq!(
        choices.len(),
        13,
        "10 commons + 3 uncommons, with only the ante rare withheld: {names:?}"
    );
    assert!(names.iter().any(|name| name.starts_with("Test Common")));
    assert!(names.iter().any(|name| name.starts_with("Test Uncommon")));
}

/// CR 407.2: playing for ante makes the class legal again. Paired with the test
/// above on the same fixture, so the exclusion is proven to follow the declared
/// policy rather than being a blanket filter on the card.
#[test]
fn booster_pack_offers_an_ante_card_when_the_format_plays_for_ante() {
    let choices = open_ante_pack_under(AntePolicy::Enabled);
    let names = choice_names(&choices);

    assert!(
        names.iter().any(|name| name == ANTE_CARD),
        "playing for ante, the pack's ante card is selectable again, got {names:?}"
    );
    assert_eq!(choices.len(), 14, "the whole pack: {names:?}");
}

/// Cast Booster Tutor and stop at the pack choice.
fn cast_and_open_pack() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(1));
    let mut runner = scenario.build();
    runner.state_mut().booster_shelf = Arc::new(test_shelf());
    let outcome = runner.cast(tutor).resolve();
    (GameRunner::from_state(outcome.state().clone()), tutor)
}

#[test]
fn booster_tutor_opens_a_pack_and_offers_every_revealed_card() {
    let (runner, _tutor) = cast_and_open_pack();

    let WaitingFor::OutsideGameChoice {
        player,
        choices,
        count,
        up_to,
        destination,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "opening a pack must raise an outside-the-game choice, got {:?}",
            runner.state().waiting_for
        );
    };

    assert_eq!(player, P0, "the spell's controller opens the pack");
    // CR 400.11b: "put ONE of them into your hand" — exactly one, not "up to".
    assert_eq!(count, 1);
    assert!(!up_to);
    assert_eq!(destination, Zone::Hand);

    // The whole pack is offered: the modern draft-booster skeleton.
    assert_eq!(choices.len(), 14, "10 commons + 3 uncommons + 1 rare");

    let mut names: Vec<&str> = Vec::new();
    for choice in &choices {
        let OutsideGameChoiceSource::BoosterPack { origin, card, .. } = &choice.source else {
            panic!("every candidate comes from the opened pack, got {choice:?}");
        };
        assert_eq!(*origin, PackOrigin::Set(PACK_SET.to_string()));
        assert_eq!(choice.count, 1, "each pack card is one physical card");
        names.push(card.name.as_str());
    }
    let distinct: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        names.len(),
        "a pack never contains the same card twice: {names:?}"
    );
}

#[test]
fn taking_one_card_puts_only_that_card_into_hand_and_removes_the_rest() {
    let (mut runner, tutor) = cast_and_open_pack();

    let WaitingFor::OutsideGameChoice { choices, .. } = runner.state().waiting_for.clone() else {
        panic!("expected the pack choice");
    };
    let (taken_slot, taken_name) = choices
        .iter()
        .find_map(|choice| match &choice.source {
            OutsideGameChoiceSource::BoosterPack {
                pack_slot, card, ..
            } => Some((*pack_slot, card.name.clone())),
            _ => None,
        })
        .expect("the pack offers at least one card");
    let objects_before = runner.state().objects.len();

    runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::BoosterPack {
                pack_slot: taken_slot,
            }],
        })
        .expect("taking one card from the opened pack is legal");

    // CR 400.11b: the taken card is now a real card in the controller's hand.
    let hand: Vec<&str> = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .map(|object| object.name.as_str())
        .collect();
    assert!(
        hand.contains(&taken_name.as_str()),
        "the chosen card must be in hand, hand is {hand:?}"
    );

    // CR 400.11: the other thirteen cards were never in a zone. Exactly ONE new
    // object exists — the taken card — and nothing landed in exile or a
    // graveyard. The graveyard check excludes the resolving spell itself.
    assert_eq!(
        runner.state().objects.len(),
        objects_before + 1,
        "only the taken card becomes an object"
    );
    assert!(
        runner.state().exile.is_empty(),
        "unchosen pack cards are not exiled"
    );
    let graveyard: Vec<&str> = runner.state().players[P0.0 as usize]
        .graveyard
        .iter()
        .filter(|id| **id != tutor)
        .filter_map(|id| runner.state().objects.get(id))
        .map(|object| object.name.as_str())
        .collect();
    assert!(
        graveyard.is_empty(),
        "unchosen pack cards are not put into a graveyard, found {graveyard:?}"
    );
}

#[test]
fn a_pack_slot_that_was_not_offered_is_rejected() {
    let (mut runner, _tutor) = cast_and_open_pack();

    let result = runner.act(GameAction::ChooseOutsideGameCards {
        selections: vec![OutsideGameSelection::BoosterPack { pack_slot: 999 }],
    });
    assert!(
        result.is_err(),
        "a slot outside the opened pack is not a legal selection"
    );
}

/// CR 400.11 + CR 609.3: an empty shelf — the shape an AI worker holding a
/// game-scoped card subset sees — opens no pack and does as much as possible
/// (nothing), rather than failing the resolution or hanging on a prompt.
#[test]
fn an_unstocked_shelf_opens_no_pack_and_leaves_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(1));
    let mut runner = scenario.build();
    runner.state_mut().booster_shelf = Arc::new(BoosterShelf::default());

    let outcome = runner.cast(tutor).resolve();
    outcome.assert_zone(&[tutor], Zone::Graveyard);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "an unstocked shelf must not leave a dangling prompt, got {:?}",
        outcome.final_waiting_for()
    );
}

fn cube_database() -> CardDatabase {
    let mut entries = serde_json::Map::new();
    for i in 0..20 {
        let card = face(&format!("Cube {i}"));
        entries.insert(
            card.name.to_lowercase(),
            serde_json::to_value(card).unwrap(),
        );
    }
    for (rarity, count, label) in [
        (Rarity::Common, 20, "Common"),
        (Rarity::Uncommon, 8, "Uncommon"),
        (Rarity::Rare, 4, "Rare"),
    ] {
        for i in 0..count {
            let mut card = face(&format!("Unrelated {label} {i}"));
            card.rarities = [rarity].into_iter().collect();
            let mut entry = serde_json::to_value(&card).unwrap();
            entry["printings"] = serde_json::json!(["OTHER"]);
            entries.insert(card.name.to_lowercase(), entry);
        }
    }
    let parsed = engine::parser::oracle::parse_oracle_text(
        BOOSTER_TUTOR_ORACLE,
        "Booster Tutor",
        &[],
        &["Instant".into()],
        &[],
    );
    let tutor = CardFace {
        name: "Booster Tutor".into(),
        card_type: CardType {
            core_types: vec![CoreType::Instant],
            ..Default::default()
        },
        oracle_text: Some(BOOSTER_TUTOR_ORACLE.into()),
        abilities: parsed.abilities,
        ..Default::default()
    };
    entries.insert("booster tutor".into(), serde_json::to_value(tutor).unwrap());
    CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string()).unwrap()
}

fn loaded_cube_game(
    pool: Option<Vec<String>>,
    opener_in_deck: bool,
) -> (GameRunner, ObjectId, CardDatabase) {
    let db = cube_database();
    assert!(
        !boosters::build_shelf(&db, 1).is_empty(),
        "hostile unrelated set can fill an ordinary pack"
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P0, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, black_pool(4));
    let mut runner = scenario.build();
    let mut main = vec!["Unrelated Common 0"; 10];
    if opener_in_deck {
        main.push("Booster Tutor");
    }
    let list: DeckList = serde_json::from_value(serde_json::json!({
        "player": { "main_deck": main, "sideboard": ["Unrelated Common 1"] },
        "opponent": { "main_deck": vec!["Unrelated Common 2"; 10] },
        "booster_pack_pool": pool,
    }))
    .unwrap();
    // A replacement payload must clear an already stocked, unrelated shelf.
    runner.state_mut().booster_shelf = Arc::new(test_shelf());
    load_and_hydrate_decks(
        runner.state_mut(),
        &resolve_deck_list(&db, &list),
        Some(&db),
    );
    assert_eq!(runner.state().booster_pack_pool.as_deref(), pool.as_ref());
    (runner, tutor, db)
}

/// The number of hydrated entries on a Cube shelf; any other shelf fails.
fn cube_shelf_len(state: &GameState) -> usize {
    match &*state.booster_shelf {
        BoosterShelf::Cube(cards) => cards.len(),
        BoosterShelf::Products(products) => {
            panic!("expected a Cube shelf, got {} set products", products.len())
        }
    }
}

fn offered_names(runner: &GameRunner) -> Vec<String> {
    let WaitingFor::OutsideGameChoice { choices, .. } = &runner.state().waiting_for else {
        panic!(
            "expected actual pack choice, got {:?}",
            runner.state().waiting_for
        );
    };
    choices.iter().map(|choice| choice.name.clone()).collect()
}

#[test]
fn original_cube_load_reveal_and_selection_use_fifteen_physical_entries() {
    let mut pool: Vec<_> = (0..14).map(|i| format!("Cube {i}")).collect();
    pool.push("Cube 0".into());
    let (mut runner, tutor, _) = loaded_cube_game(Some(pool.clone()), true);
    assert_eq!(cube_shelf_len(runner.state()), 15);
    assert!(matches!(
        game_requires_full_card_db(runner.state()),
        Some(FullDbReason::BoosterPack)
    ));
    let outcome = runner.cast(tutor).resolve();
    // CR 701.20: every pack entry is revealed, including duplicate occurrences.
    let revealed = outcome
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::CardsRevealed { card_names, .. } => Some(card_names.clone()),
            _ => None,
        })
        .expect("the actual spell reveals its opened pack");
    assert_eq!(revealed.len(), 15);
    outcome.assert_hand_drawn(P0, 0);
    let WaitingFor::OutsideGameChoice {
        choices,
        count,
        destination,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!("pack choice")
    };
    assert_eq!((*count, *destination), (1, Zone::Hand));
    let mut slots = std::collections::BTreeSet::new();
    for choice in choices {
        let OutsideGameChoiceSource::BoosterPack {
            pack_slot, origin, ..
        } = &choice.source
        else {
            panic!("cube source")
        };
        assert_eq!(*origin, PackOrigin::Cube);
        slots.insert(*pack_slot);
    }
    assert_eq!(slots.len(), 15);
    let mut expected = pool.clone();
    expected.sort();
    let mut actual = revealed;
    actual.sort();
    assert_eq!(actual, expected);
    runner = GameRunner::from_state(outcome.state().clone());
    for selection in [
        OutsideGameSelection::BoosterPack { pack_slot: 999 },
        OutsideGameSelection::Sideboard { sideboard_index: 0 },
    ] {
        assert!(runner
            .act(GameAction::ChooseOutsideGameCards {
                selections: vec![selection]
            })
            .is_err());
    }
    let before = runner.state().objects.len();
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::BoosterPack { pack_slot: 0 }],
        })
        .unwrap();
    // CR 400.11 + CR 400.11b: only the selected entry enters a game zone.
    assert_eq!(runner.state().objects.len(), before + 1);
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
    assert_eq!(runner.state().booster_pack_pool.as_deref(), Some(&pool));
}

#[test]
fn cube_source_sizes_refill_without_replacement_and_empty_or_missing_stay_bounded() {
    for size in [1, 14, 15, 16, 20] {
        let pool: Vec<_> = (0..size).map(|i| format!("Cube {i}")).collect();
        let (mut runner, tutor, _) = loaded_cube_game(Some(pool.clone()), true);
        let outcome = runner.cast(tutor).resolve();
        runner = GameRunner::from_state(outcome.state().clone());
        let names = offered_names(&runner);
        assert_eq!(names.len(), 15, "source size {size}");
        assert!(names.iter().all(|name| pool.contains(name)));
        let distinct: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(distinct.len(), size.min(15));
    }
    for pool in [Vec::new(), vec!["Cube 0".into(), "Missing entry".into()]] {
        let (mut runner, tutor, _) = loaded_cube_game(Some(pool), true);
        assert_eq!(
            *runner.state().booster_shelf,
            BoosterShelf::Cube(Vec::new())
        );
        let mut expected_rng = runner.state().rng.clone();
        let mut direct_rng = runner.state().rng.clone();
        assert!(boosters::open_pack(&runner.state().booster_shelf, &mut direct_rng).is_none());
        assert_eq!(direct_rng.next_u64(), expected_rng.next_u64());
        let outcome = runner.cast(tutor).resolve();
        outcome.assert_zone(&[tutor], Zone::Graveyard);
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::OpenBoosterPack,
                ..
            }
        )));
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn ordinary_deck_loading_still_stocks_and_opens_a_fourteen_card_set_pack() {
    let (mut runner, tutor, _) = loaded_cube_game(None, true);
    assert!(matches!(
        &*runner.state().booster_shelf,
        BoosterShelf::Products(products) if !products.is_empty()
    ));
    let outcome = runner.cast(tutor).resolve();
    runner = GameRunner::from_state(outcome.state().clone());
    let names = offered_names(&runner);
    assert_eq!(names.len(), 14);
    assert!(names.iter().all(|name| name.starts_with("Unrelated ")));
}

/// A pack deals from `booster_pack_pool`, so an opener that exists only there
/// can enter the game only from a pack something already in the game opened.
/// Scanning the pool would stock the shelf, and widen the AI worker to the
/// full card database, for every Cube that merely contains an opener.
///
/// REVERT-PROBE: scan the `booster_pack_pool` names again in
/// `boosters::game_opens_booster_packs` and the pool-only half reds.
#[test]
fn an_opener_only_in_the_original_pool_does_not_stock_the_shelf() {
    let pool = vec!["Booster Tutor".to_string(), "Cube 0".to_string()];
    let (pool_only, tutor, db) = loaded_cube_game(Some(pool.clone()), false);
    // The cast-ready tutor is a scenario object with no printed face, so no
    // object or deck entry the scan reads names an opener.
    assert!(pool_only.state().objects[&tutor].printed_ref.is_none());
    assert!(!boosters::game_opens_booster_packs(pool_only.state(), &db));
    assert!(pool_only.state().booster_shelf.is_empty());
    assert!(game_requires_full_card_db(pool_only.state()).is_none());

    // Reach-guard: the same source with the opener in a deck stocks the Cube
    // and escalates, so the half above is the pool scan's absence at work.
    let (in_deck, _, _) = loaded_cube_game(Some(pool.clone()), true);
    assert_eq!(cube_shelf_len(in_deck.state()), pool.len());
    assert!(matches!(
        game_requires_full_card_db(in_deck.state()),
        Some(FullDbReason::BoosterPack)
    ));
}

#[test]
fn restoring_a_cube_game_rebuilds_its_shelf_without_advancing_the_game_rng() {
    let pool = vec![
        "Booster Tutor".into(),
        "Cube 0".into(),
        "Cube 0".into(),
        "Cube 19".into(),
    ];
    let (mut runner, tutor, db) = loaded_cube_game(Some(pool.clone()), true);
    assert_eq!(cube_shelf_len(runner.state()), pool.len());
    runner.state_mut().capture_rng_word_pos();
    let json = serde_json::to_string(runner.state()).unwrap();
    let mut restored: GameState = serde_json::from_str(&json).unwrap();
    restored.rehydrate_rng();
    assert!(restored.booster_shelf.is_empty());
    assert_eq!(restored.booster_pack_pool.as_deref(), Some(&pool));
    let mut before = restored.rng.clone();
    rehydrate_game_from_card_db(&mut restored, &db);
    rehydrate_game_from_card_db(&mut restored, &db);
    assert_eq!(restored.rng.clone().next_u64(), before.next_u64());
    let mut original = runner;
    let mut restored = GameRunner::from_state(restored);
    for _ in 0..2 {
        let a = original.cast(tutor).resolve();
        let b = restored.cast(tutor).resolve();
        original = GameRunner::from_state(a.state().clone());
        restored = GameRunner::from_state(b.state().clone());
        assert_eq!(offered_names(&original), offered_names(&restored));
        for game in [&mut original, &mut restored] {
            game.act(GameAction::ChooseOutsideGameCards {
                selections: vec![OutsideGameSelection::BoosterPack { pack_slot: 0 }],
            })
            .unwrap();
            engine::game::zones::move_to_zone(game.state_mut(), tutor, Zone::Hand, &mut Vec::new());
            game.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
        }
    }
}

#[test]
fn identical_decks_with_different_original_cubes_have_distinct_state_identity() {
    let (a, _, _) = loaded_cube_game(Some(vec!["Cube 0".into()]), true);
    let (same, _, _) = loaded_cube_game(Some(vec!["Cube 0".into()]), true);
    let (other, _, _) = loaded_cube_game(Some(vec!["Cube 19".into()]), true);
    assert_eq!(a.state().deck_pools, other.state().deck_pools);
    assert_eq!(a.state(), same.state());
    assert_ne!(a.state(), other.state());
}

#[test]
fn player_one_owns_the_card_they_take_from_the_cube() {
    let db = cube_database();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario
        .add_spell_to_hand_from_oracle(P1, "Booster Tutor", true, BOOSTER_TUTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P1, black_pool(1));
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.state_mut().booster_shelf =
        Arc::new(boosters::build_pool_shelf(&db, &["Cube 0".into()]));
    let outcome = runner.cast(tutor).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::OutsideGameChoice { player: P1, .. }
    ));
    runner = GameRunner::from_state(outcome.state().clone());
    assert_eq!(offered_names(&runner), vec!["Cube 0"; 15]);
    runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::BoosterPack { pack_slot: 7 }],
        })
        .unwrap();
    // CR 108.3: the player bringing a card in from outside the game owns it.
    let card = runner.state().players[1]
        .hand
        .iter()
        .find_map(|id| {
            let object = &runner.state().objects[id];
            (object.name == "Cube 0").then_some(object)
        })
        .expect("player one received the chosen card");
    assert_eq!(card.owner, P1);
    assert_eq!(card.controller, P1);
}

/// Concede game one of a Bo3 built by [`loaded_cube_game`] and play through
/// both sideboard submissions and the play/draw choice into game two.
fn concede_into_game_two(runner: &mut GameRunner) {
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesSideboard { player: P0, .. }
    ));
    let count = |name: &str, count| DeckCardCount {
        name: name.into(),
        count,
    };
    runner
        .act(GameAction::SubmitSideboard {
            main: vec![
                count("Unrelated Common 0", 9),
                count("Unrelated Common 1", 1),
                count("Booster Tutor", 1),
            ],
            sideboard: vec![count("Unrelated Common 0", 1)],
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesSideboard { player: P1, .. }
    ));
    runner
        .act(GameAction::SubmitSideboard {
            main: vec![count("Unrelated Common 2", 10)],
            sideboard: vec![],
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BetweenGamesChoosePlayDraw { .. }
    ));
    runner
        .act(GameAction::ChoosePlayDraw { play_first: false })
        .unwrap();
    assert_eq!(runner.state().game_number, 2);
}

/// Put game two's Booster Tutor into player zero's hand with priority in a main
/// phase, cast it, and return the resolved game.
fn cast_game_two_tutor(mut runner: GameRunner) -> GameRunner {
    let tutor = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Booster Tutor")
        .unwrap()
        .id;
    engine::game::zones::move_to_zone(runner.state_mut(), tutor, Zone::Hand, &mut Vec::new());
    runner.state_mut().phase = Phase::PreCombatMain;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.state_mut().priority_player = P0;
    let outcome = runner.cast(tutor).resolve();
    GameRunner::from_state(outcome.state().clone())
}

#[test]
fn between_games_actions_preserve_cube_pool_and_hydrated_shelf_without_external_rehydrate() {
    let pool = vec!["Cube 0".into(), "Cube 19".into()];
    let (mut runner, tutor, _) = loaded_cube_game(Some(pool.clone()), true);
    runner.state_mut().match_config.match_type = MatchType::Bo3;
    let outcome = runner.cast(tutor).resolve();
    runner = GameRunner::from_state(outcome.state().clone());
    let chosen = offered_names(&runner)[0].clone();
    runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![OutsideGameSelection::BoosterPack { pack_slot: 0 }],
        })
        .unwrap();
    assert!(runner.state().players[0]
        .hand
        .iter()
        .any(|id| runner.state().objects[id].name == chosen));
    concede_into_game_two(&mut runner);
    assert_eq!(runner.state().booster_pack_pool.as_deref(), Some(&pool));
    assert_eq!(cube_shelf_len(runner.state()), pool.len());
    // CR 400.11b: the brought-in object lasts only for the game that brought it in.
    assert!(runner
        .state()
        .objects
        .values()
        .all(|object| object.name != chosen));
    let runner = cast_game_two_tutor(runner);
    assert_eq!(offered_names(&runner).len(), 15);
    assert!(offered_names(&runner)
        .iter()
        .all(|name| pool.contains(name)));
}

/// Pre-existing on main: the between-games rebuild reset the shelf, so an
/// ordinary Bo3 game two opened nothing. The first Cube carry kept only a Cube
/// shelf, which left set products broken.
///
/// REVERT-PROBE: guard the shelf carry in
/// `restart_between_games_with_starting_player` on
/// `booster_pack_pool.is_some()` again and game two's Booster Tutor resolves
/// to priority with no pack choice, which `offered_names` reports.
#[test]
fn an_ordinary_bo3_game_two_booster_tutor_opens_a_set_pack() {
    let (mut runner, _, _) = loaded_cube_game(None, true);
    runner.state_mut().match_config.match_type = MatchType::Bo3;
    assert!(
        matches!(
            &*runner.state().booster_shelf,
            BoosterShelf::Products(products) if !products.is_empty()
        ),
        "test precondition: game one stocks set products"
    );
    concede_into_game_two(&mut runner);
    assert!(runner.state().booster_pack_pool.is_none());

    let runner = cast_game_two_tutor(runner);
    let names = offered_names(&runner);
    assert_eq!(
        names.len(),
        14,
        "10 commons + 3 uncommons + 1 rare: {names:?}"
    );
    assert!(names.iter().all(|name| name.starts_with("Unrelated ")));
    let WaitingFor::OutsideGameChoice { choices, .. } = &runner.state().waiting_for else {
        unreachable!("offered_names established the pack choice");
    };
    assert!(choices.iter().all(|choice| matches!(
        &choice.source,
        OutsideGameChoiceSource::BoosterPack { origin, .. }
            if *origin == PackOrigin::Set("OTHER".to_string())
    )));
}

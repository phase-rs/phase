//! Runtime witnesses for the verbless mass-exile conjunct released by the
//! `ChangeZoneAll`-carries-"exile"-forward change in
//! `parser/oracle_effect/mod.rs` (`extract_effect_verb`'s `ChangeZoneAll`
//! arm).
//!
//! Shadows' Verdict — Sorcery, {3}{B}{B} — Oracle text:
//! "Exile all creatures and planeswalkers with mana value 3 or less from the
//! battlefield and all creature and planeswalker cards with mana value 3 or
//! less from all graveyards."
//!
//! The printed card is staged through `add_real_card` and cast through the
//! production cast pipeline (`runner.cast(..).resolve()`), paying from a real
//! mana pool. Every other body is a textless filler whose mana cost this
//! scenario API sets explicitly; none of them carries an ability, so the only
//! rules text under test is the printed sorcery's.
//!
//! CR references verified against docs/MagicCompRules.txt:
//!   - CR 202.3: mana value is the total mana in the mana cost.
//!   - CR 202.3a: an object with no mana cost has mana value 0 (the token).
//!   - CR 701.13a: to exile an object, move it to the exile zone.
//!   - CR 111.7: a token outside the battlefield ceases to exist (SBA).
//!   - CR 608.2c: the instructions are followed in the order written; the two
//!     legs of this card name disjoint zone populations.
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Shadows' Verdict is {3}{B}{B}: two black plus three generic.
fn verdict_pool() -> Vec<ManaUnit> {
    let mut kinds = Vec::new();
    kinds.extend([ManaType::Black; 2]);
    kinds.extend([ManaType::Colorless; 3]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9_000 + i as u64), false, vec![]))
        .collect()
}

/// The staged objects of one player, split by the zone they must end in.
struct Board {
    mv1_creature: ObjectId,
    mv3_creature: ObjectId,
    mv4_creature: ObjectId,
    mv3_planeswalker: ObjectId,
    mv5_planeswalker: ObjectId,
    land: ObjectId,
    token: ObjectId,
    gy_mv2_creature: ObjectId,
    gy_mv5_creature: ObjectId,
    gy_mv3_planeswalker: ObjectId,
    gy_mv5_planeswalker: ObjectId,
    gy_instant: ObjectId,
}

/// Stage one player's half of the population: three battlefield creatures
/// (MV 1 / 3 / 4), two battlefield planeswalkers (MV 3 / 5), a noncreature
/// nonplaneswalker permanent, a 0-mana token creature, and a graveyard holding
/// creature cards (MV 2 / 5), planeswalker cards (MV 3 / 5) and an instant card
/// whose mana value is 3 — every boundary of the printed filter is populated.
fn stage_board(scenario: &mut GameScenario, player: PlayerId, tag: &str) -> Board {
    let mv1_creature = scenario
        .add_creature(player, &format!("{tag} MV1 Creature"), 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mv3_creature = scenario
        .add_creature(player, &format!("{tag} MV3 Creature"), 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mv4_creature = scenario
        .add_creature(player, &format!("{tag} MV4 Creature"), 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mv3_planeswalker = scenario
        .add_creature(player, &format!("{tag} MV3 Walker"), 0, 0)
        .as_planeswalker_with_loyalty("Walker", 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mv5_planeswalker = scenario
        .add_creature(player, &format!("{tag} MV5 Walker"), 0, 0)
        .as_planeswalker_with_loyalty("Walker", 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    // A basic land is a noncreature, nonplaneswalker permanent with mana value 0
    // (CR 202.3a): the leg that names creatures and planeswalkers must not
    // widen to "every permanent".
    let land = scenario.add_basic_land(player, ManaColor::Green);
    // No mana cost: mana value 0 (CR 202.3a), so the token is inside the bound.
    let token = scenario
        .add_creature(player, &format!("{tag} Token"), 2, 2)
        .id();

    let gy_mv2_creature = scenario
        .add_creature_to_graveyard(player, &format!("{tag} Grave MV2 Creature"), 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let gy_mv5_creature = scenario
        .add_creature_to_graveyard(player, &format!("{tag} Grave MV5 Creature"), 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let gy_mv3_planeswalker = scenario
        .add_creature_to_graveyard(player, &format!("{tag} Grave MV3 Walker"), 0, 0)
        .as_planeswalker_with_loyalty("Walker", 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let gy_mv5_planeswalker = scenario
        .add_creature_to_graveyard(player, &format!("{tag} Grave MV5 Walker"), 0, 0)
        .as_planeswalker_with_loyalty("Walker", 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    // An instant card with mana value 3: inside the bound by mana value, outside
    // it by card type.
    let gy_instant = scenario
        .add_spell_to_graveyard(player, &format!("{tag} Grave Instant"), true)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    Board {
        mv1_creature,
        mv3_creature,
        mv4_creature,
        mv3_planeswalker,
        mv5_planeswalker,
        land,
        token,
        gy_mv2_creature,
        gy_mv5_creature,
        gy_mv3_planeswalker,
        gy_mv5_planeswalker,
        gy_instant,
    }
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|id| id.0);
    ids
}

fn battlefield_of(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, obj)| obj.zone == Zone::Battlefield && obj.controller == player)
        .map(|(id, _)| *id)
        .collect()
}

fn graveyard_of(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state.players[player.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect()
}

fn exile_of(state: &GameState) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, obj)| obj.zone == Zone::Exile)
        .map(|(id, _)| *id)
        .collect()
}

/// The completion boundary for a spell with no target and no choice slot:
/// nothing on the stack, and the active player holds priority again.
fn assert_settled(outcome: &Outcome, player: PlayerId, what: &str) {
    assert!(
        outcome.state().stack.is_empty(),
        "{what}: the stack must be empty after resolution, {} entry/s remain",
        outcome.state().stack.len()
    );
    let waiting = outcome.final_waiting_for();
    assert!(
        matches!(waiting, WaitingFor::Priority { player: holder } if *holder == player),
        "{what}: expected {player:?}'s priority, got {waiting:?}"
    );
}

/// Cast Shadows' Verdict in P0's precombat main phase with a funded pool, and
/// return the resolved outcome plus both players' staged boards.
fn cast_verdict() -> (Outcome, Board, Board, ObjectId) {
    let db = load_db().expect("the committed integration card fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let verdict = scenario.add_real_card(P0, "Shadows' Verdict", Zone::Hand, db);
    let p0 = stage_board(&mut scenario, P0, "P0");
    let p1 = stage_board(&mut scenario, P1, "P1");
    scenario.with_mana_pool(P0, verdict_pool());
    let mut runner: GameRunner = scenario.build();

    // CR 111.7 + CR 202.3a: a token permanent has mana value 0.
    for token in [p0.token, p1.token] {
        runner
            .state_mut()
            .objects
            .get_mut(&token)
            .expect("staged token")
            .is_token = true;
    }

    let outcome = runner.cast(verdict).resolve();
    (outcome, p0, p1, verdict)
}

/// Shadows' Verdict exiles, from BOTH battlefields and BOTH graveyards, every
/// creature/planeswalker at mana value 3 or less and nothing else.
///
/// Discriminators, per player: on the battlefield MV 1 and MV 3 leave while
/// MV 4 stays, and the MV 3 planeswalker leaves while the MV 5 one stays, while
/// the 0-mana land and the 0-mana token prove the filter is by type as well as
/// by mana value; in the graveyard MV 2 leaves while MV 5 stays, the MV 3
/// planeswalker card leaves while the MV 5 one stays, and the MV 3 instant card
/// stays. A swapped bound, a missing type axis, or a one-player scope all yield
/// a different zone membership.
#[test]
fn shadows_verdict_exiles_mv3_or_less_creatures_and_planeswalkers_in_both_players_zones() {
    let (outcome, p0, p1, verdict) = cast_verdict();
    let state = outcome.state();

    // CR 701.13a: each named permanent/card moved to the exile zone.
    for board in [&p0, &p1] {
        outcome.assert_zone(
            &[
                board.mv1_creature,
                board.mv3_creature,
                board.mv3_planeswalker,
                board.gy_mv2_creature,
                board.gy_mv3_planeswalker,
            ],
            Zone::Exile,
        );
    }

    // Exact battlefield membership: only the MV 4 creature, the MV 5
    // planeswalker and the land survive, for each player.
    assert_eq!(
        sorted(battlefield_of(state, P0)),
        sorted(vec![p0.mv4_creature, p0.mv5_planeswalker, p0.land]),
        "P0 battlefield: MV 4 creature, MV 5 planeswalker and the land survive"
    );
    assert_eq!(
        sorted(battlefield_of(state, P1)),
        sorted(vec![p1.mv4_creature, p1.mv5_planeswalker, p1.land]),
        "P1 battlefield: MV 4 creature, MV 5 planeswalker and the land survive"
    );

    // Exact graveyard membership: the MV 5 creature card, the MV 5 planeswalker
    // card and the MV 3 instant card stay; the resolved sorcery itself is a
    // noncreature, nonplaneswalker card and stays too.
    assert_eq!(
        sorted(graveyard_of(state, P0)),
        sorted(vec![
            p0.gy_mv5_creature,
            p0.gy_mv5_planeswalker,
            p0.gy_instant,
            verdict,
        ]),
        "P0 graveyard keeps the MV 5 creature/planeswalker cards, the instant and the sorcery"
    );
    assert_eq!(
        sorted(graveyard_of(state, P1)),
        sorted(vec![
            p1.gy_mv5_creature,
            p1.gy_mv5_planeswalker,
            p1.gy_instant
        ]),
        "P1 graveyard keeps the MV 5 creature/planeswalker cards and the instant"
    );

    // Exact exile membership: the ten named permanents/cards. The exiled tokens
    // are not here — CR 111.7 removes them from the game instead.
    let mut expected_exile: Vec<ObjectId> = Vec::new();
    for board in [&p0, &p1] {
        expected_exile.extend([
            board.mv1_creature,
            board.mv3_creature,
            board.mv3_planeswalker,
            board.gy_mv2_creature,
            board.gy_mv3_planeswalker,
        ]);
    }
    assert_eq!(
        sorted(exile_of(state)),
        sorted(expected_exile),
        "nothing else is exiled"
    );

    // CR 111.7: a token that left the battlefield ceases to exist.
    for board in [&p0, &p1] {
        assert!(
            !state.objects.contains_key(&board.token),
            "the token creature is gone from the game, not parked in another zone"
        );
    }

    assert_settled(&outcome, P0, "Shadows' Verdict");
}

/// Hostile sibling: the two legs name the battlefield and the graveyards only.
/// A creature card with mana value 3 or less sitting in a hand or in a library is
/// untouched, while the same-shaped cards in the named zones are exiled in the
/// same resolution (the reach guard; without it this test would pass vacuously
/// on a card that never resolved).
#[test]
fn shadows_verdict_leaves_mv3_or_less_creature_cards_in_hand_and_library_alone() {
    let db = load_db().expect("the committed integration card fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let verdict = scenario.add_real_card(P0, "Shadows' Verdict", Zone::Hand, db);

    // Reach guards: one creature permanent and one creature card in the zones
    // the card does name.
    let battlefield_two = scenario
        .add_creature(P0, "Battlefield MV2 Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let graveyard_two = scenario
        .add_creature_to_graveyard(P1, "Graveyard MV2 Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    // Hostile controls: MV 3-or-less creature cards in zones neither leg names.
    let p0_hand_two = scenario
        .add_creature_to_hand(P0, "P0 Hand MV2 Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let p1_hand_three = scenario
        .add_creature_to_hand(P1, "P1 Hand MV3 Creature", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let library_two = scenario
        .add_creature_to_hand(P0, "P0 Library MV2 Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    scenario.with_mana_pool(P0, verdict_pool());
    let mut runner = scenario.build();

    // Re-seat the staged library card as P0's library top. A hidden-zone card is
    // not on the battlefield and not in a graveyard, so no leg may reach it.
    {
        let state = runner.state_mut();
        state.players[P0.0 as usize]
            .hand
            .retain(|&id| id != library_two);
        state
            .objects
            .get_mut(&library_two)
            .expect("staged library card")
            .zone = Zone::Library;
        state.players[P0.0 as usize].library.insert(0, library_two);
    }

    let outcome = runner.cast(verdict).resolve();
    let state = outcome.state();

    // Reach guard: the same shape in the named zones is exiled, and those two
    // are the only objects the card exiles here.
    outcome.assert_zone(&[battlefield_two, graveyard_two], Zone::Exile);
    assert_eq!(
        sorted(exile_of(state)),
        sorted(vec![battlefield_two, graveyard_two]),
        "the card exiles exactly the MV 2 creature permanent and the MV 2 creature card"
    );

    // The named-zone legs do not leak into hand or library.
    outcome.assert_zone(&[p0_hand_two, p1_hand_three], Zone::Hand);
    outcome.assert_zone(&[library_two], Zone::Library);
    assert_eq!(
        sorted(state.players[P0.0 as usize].hand.iter().copied().collect()),
        sorted(vec![p0_hand_two]),
        "P0's hand holds only the staged MV 2 creature card"
    );
    assert_eq!(
        sorted(state.players[P1.0 as usize].hand.iter().copied().collect()),
        sorted(vec![p1_hand_three]),
        "P1's hand holds only the staged MV 3 creature card"
    );
    assert_eq!(
        state.players[P0.0 as usize].library.front(),
        Some(&library_two),
        "the MV 2 creature card stays on top of P0's library"
    );

    assert_settled(&outcome, P0, "Shadows' Verdict zone boundary");
}

/// Once More with Feeling — Sorcery, {W}{W}{W}{W}: "Exile all permanents and all cards from
/// all graveyards. Each player shuffles their hand into their library, then draws seven
/// cards. Each player's life total becomes 10. Exile Once More with Feeling."
///
/// The first sentence is the same verbless-conjunct shape as Shadows' Verdict ("exile all A
/// and all B"); the witness drives the WHOLE printed card: both players' permanents and
/// graveyard cards are exiled, each hand is shuffled into the library and seven cards are
/// drawn from it, both life totals become 10 (from different starting totals), and the
/// spell exiles itself instead of resting in the graveyard.
#[test]
fn once_more_with_feeling_exiles_permanents_and_graveyards_then_resets_hands_and_life() {
    let db = load_db().expect("the committed integration card fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let feeling = scenario.add_real_card(P0, "Once More with Feeling", Zone::Hand, db);
    scenario.with_life(P0, 3);
    scenario.with_life(P1, 25);

    let mut permanents = Vec::new();
    let mut graveyard_cards = Vec::new();
    let mut shuffled_pool = Vec::new();
    for (player, tag) in [(P0, "P0"), (P1, "P1")] {
        permanents.push(
            scenario
                .add_creature(player, &format!("{tag} Bear"), 2, 2)
                .id(),
        );
        permanents.push(scenario.add_basic_land(player, ManaColor::Green));
        graveyard_cards.push(
            scenario
                .add_creature_to_graveyard(player, &format!("{tag} Dead Bear"), 2, 2)
                .id(),
        );
        shuffled_pool.push(
            scenario
                .add_creature_to_hand(player, &format!("{tag} Hand Bear"), 1, 1)
                .id(),
        );
        for n in 0..9 {
            shuffled_pool.push(scenario.add_card_to_library_top(player, &format!("{tag} Lib {n}")));
        }
    }
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|i| ManaUnit::new(ManaType::White, ObjectId(9_100 + i), false, vec![]))
            .collect(),
    );
    let mut runner: GameRunner = scenario.build();

    let outcome = runner.cast(feeling).resolve();
    let state = outcome.state();

    // Every permanent and every graveyard card of both players is in exile, and the
    // spell exiled itself (it is not in a graveyard).
    outcome.assert_zone(&permanents, Zone::Exile);
    outcome.assert_zone(&graveyard_cards, Zone::Exile);
    outcome.assert_zone(&[feeling], Zone::Exile);
    for player in [P0, P1] {
        assert!(
            graveyard_of(state, player).is_empty(),
            "{player:?}'s graveyard is empty after the exile and the self-exile"
        );
        assert!(
            battlefield_of(state, player).is_empty(),
            "{player:?} controls no permanents"
        );
    }
    // Each player shuffled the hand into the library and drew seven: hand holds seven cards
    // and hand plus library still hold exactly that player's ten staged cards.
    for player in [P0, P1] {
        let seat = &state.players[player.0 as usize];
        assert_eq!(seat.hand.len(), 7, "{player:?} drew seven cards");
        let mut held: Vec<ObjectId> = seat.hand.iter().copied().collect();
        held.extend(seat.library.iter().copied());
        let mut staged: Vec<ObjectId> = shuffled_pool
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|o| o.owner == player))
            .collect();
        assert_eq!(sorted(held), sorted(std::mem::take(&mut staged)));
        assert_eq!(seat.life, 10, "{player:?}'s life total becomes 10");
    }
    assert_settled(&outcome, P0, "Once More with Feeling");
}

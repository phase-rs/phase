//! Runtime witnesses for the mass-exile population union
//! (`try_parse_mass_exile_permanents_and_zones` in `parser/oracle_effect/imperative.rs`),
//! which reads "exile all A and all B" as ONE `ChangeZoneAll` whose legs each keep
//! their own zone, so both populations leave in one simultaneous event.
//!
//! Shadows' Verdict — Sorcery, {3}{B}{B} — Oracle text:
//! "Exile all creatures and planeswalkers with mana value 3 or less from the
//! battlefield and all creature and planeswalker cards with mana value 3 or
//! less from all graveyards."
//!
//! The printed card is staged through `add_real_card` and cast through the
//! production cast pipeline (`runner.cast(..).resolve()`), paying from a real
//! mana pool. Every other body is a textless filler whose mana cost this
//! scenario API sets explicitly, so the only rules text under test is the
//! printed sorcery's. The mass-exile witnesses below add a second printed body,
//! Stonebound Mentor, whose leaves-your-graveyard ability is the subject.
//!
//! Decree of Annihilation — Sorcery, {8}{R}{R} — is a cross-zone union of three
//! populations whose parse the union recognizer now builds with `origin: None` (it
//! was a single origin of Graveyard before; the resolver already read the zones
//! from the filter, so this witness guards the behavior and is not a reverse-fail):
//! "Exile all artifacts, creatures, and lands from the battlefield, all cards from all graveyards, and all cards from
//! all hands." Its witness drives the whole printed instruction as ONE
//! `ChangeZoneAll`.
//!
//! CR references verified against docs/MagicCompRules.txt:
//!   - CR 202.3: mana value is the total mana in the mana cost.
//!   - CR 202.3a: an object with no mana cost has mana value 0 (the token).
//!   - CR 701.13a: to exile an object, move it to the exile zone.
//!   - CR 111.7: a token outside the battlefield ceases to exist (SBA).
//!   - CR 608.2c: the instructions are followed in the order written; the two
//!     legs of this card name disjoint zone populations.
//!   - CR 608.2f: an action taken on multiple players and/or objects is
//!     processed simultaneously; the two legs are one zone-change event.
//!   - CR 608.2n: as the final part of an instant or sorcery spell's resolution,
//!     the spell is put into its owner's graveyard — so a resolving sorcery is on
//!     the stack, not in a graveyard, while its own instruction runs.
//!   - CR 603.10a: a "leaves your graveyard" trigger looks back in time to the
//!     game state immediately before its event.
//!   - CR 701.22a: Scry N looks at the top N cards, then puts any number of
//!     them on the bottom; the rest stay on top in any order.
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
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

/// The completion boundary of a manually driven resolution: nothing on the
/// stack and `player` holding priority again. Mirrors [`assert_settled`] for a
/// witness that must see the scry prompt before the harness answers it, and
/// panics on any intervening prompt so a second trigger cannot pass as
/// "settled".
fn settle_at_priority(
    runner: &mut GameRunner,
    player: PlayerId,
    what: &str,
    events: &mut Vec<GameEvent>,
) {
    for _ in 0..8 {
        if runner.state().stack.is_empty()
            && matches!(
                &runner.state().waiting_for,
                WaitingFor::Priority { player: _ }
            )
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { player: _ } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("a priority pass must be accepted");
                events.extend(result.events);
            }
            other => panic!("{what}: expected only priority while settling, got {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "{what}: the stack must be empty after resolution, {} entry/s remain",
        runner.state().stack.len()
    );
    let waiting = &runner.state().waiting_for;
    assert!(
        matches!(waiting, WaitingFor::Priority { player: holder } if *holder == player),
        "{what}: expected {player:?}'s priority, got {waiting:?}"
    );
}

/// Pass priority until the scry prompt is offered to `player`, or the stack
/// drains. `None` means no ability ever prompted — the ability did not trigger.
/// Returns the look-at set the engine offered (CR 701.22a). Panics on any other
/// prompt: neither printed sorcery below has a further decision point.
fn resolve_to_scry_prompt(
    runner: &mut GameRunner,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Option<Vec<ObjectId>> {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { player: _ } => {
                if runner.state().stack.is_empty() {
                    return None;
                }
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("a priority pass must be accepted");
                events.extend(result.events);
            }
            WaitingFor::ScryChoice { player: who, cards } if who == player => return Some(cards),
            other => panic!("unexpected prompt before the scry choice: {other:?}"),
        }
    }
    panic!("resolution never reached a stable point");
}

/// CR 701.22a: each Scry instruction from `source` that is applied pushes
/// exactly one `EffectResolved { kind: Scry }` (the controller's top/bottom
/// choice may still be pending, but the completion is driven by the witness).
/// Counting those is the public count of the ability resolving, independent of
/// how many prompts the harness drove by hand.
fn scry_resolutions_from(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { kind: EffectKind::Scry, source_id, subject: None }
                    if *source_id == source
            )
        })
        .count()
}

/// Positions of the `EffectResolved { kind: ChangeZoneAll }` events `source`
/// published — one per mass-exile instruction it resolved.
fn change_zone_all_resolutions_from(events: &[GameEvent], source: ObjectId) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                GameEvent::EffectResolved { kind: EffectKind::ChangeZoneAll, source_id, subject: None }
                    if *source_id == source
            )
        })
        .map(|(index, _)| index)
        .collect()
}

/// Position of the action event that moved `object` from `from` to `to`.
/// `None` when the public event stream does not record that move at all.
fn zone_change_event_index(
    events: &[GameEvent],
    object: ObjectId,
    from: Zone,
    to: Zone,
) -> Option<usize> {
    events.iter().position(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(event_from), to: event_to, record }
                if *object_id == object && *event_from == from && *event_to == to
                    && record.object_id == object
        )
    })
}

/// Two-player game in P0's precombat main: Stonebound Mentor on P0's
/// battlefield, Grizzly Bears in `bears_player`'s graveyard, a four-card P0
/// library, and a funded Shadows' Verdict in P0's hand. Returns the built
/// runner, the three printed cards, and P0's library order at build time.
fn mentor_verdict_game(
    bears_player: PlayerId,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let db = load_db().expect("the committed integration card fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let verdict = scenario.add_real_card(P0, "Shadows' Verdict", Zone::Hand, db);
    // Stonebound Mentor — {1}{R}{W} Creature - Spirit Advisor 3/3, mana value 3
    // (CR 202.3), so the printed battlefield leg exiles it.
    let mentor = scenario.add_real_card(P0, "Stonebound Mentor", Zone::Battlefield, db);
    // Grizzly Bears — {1}{G} Creature - Bear 2/2, mana value 2.
    let bears = scenario.add_real_card(bears_player, "Grizzly Bears", Zone::Graveyard, db);
    for n in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {n}"));
    }
    scenario.with_mana_pool(P0, verdict_pool());
    let runner = scenario.build();
    let library_before = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    (runner, verdict, mentor, bears, library_before)
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
/// The first sentence is the same population-union shape as Shadows' Verdict ("exile all A
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

/// CR 608.2f + CR 603.10a: "Exile all creatures and planeswalkers … from the
/// battlefield and all creature and planeswalker cards … from all graveyards"
/// is one action taken on multiple objects, so it is one simultaneous
/// zone-change event. Stonebound Mentor is exiled by the battlefield leg, and
/// its "Whenever one or more cards leave your graveyard" ability looks back in
/// time to the board immediately before that event, where the Grizzly Bears was
/// still in P0's graveyard: exactly one scry.
///
/// Driven by hand only because the harness's default answer for a scry would
/// hide the prompt (CR 701.22a); every step is a public action.
/// Discriminators:
///   * both populations are exiled together (Mentor battlefield -> exile, Bears
///     graveyard -> exile), and no bystander survives in either zone;
///   * the scry prompt is offered with the library's top card, and the ability
///     resolves exactly once while its source is already in exile.
///
/// The engine's internal `LogicalZoneChangeGroupId` is not part of `GameEvent`,
/// so there is no public group id to assert. The simultaneity is witnessed by
/// its only public consequence: a chained (two-event) resolution removes the
/// Mentor in the earlier event, so the graveyard leg can no longer source the
/// ability and the count would be 0. The one-trigger count is therefore the
/// public discriminator between the simultaneous and chained executions.
#[test]
fn shadows_verdict_exiling_the_observer_and_a_graveyard_card_triggers_one_scry() {
    let (mut runner, verdict, mentor, bears, library_before) = mentor_verdict_game(P0);
    drop(runner.cast(verdict).commit());
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == verdict),
        "reach guard: Shadows' Verdict is on the stack after the cast"
    );

    let mut events: Vec<GameEvent> = Vec::new();
    let offered = resolve_to_scry_prompt(&mut runner, P0, &mut events).expect(
        "CR 603.10a + CR 608.2f: Stonebound Mentor must trigger once for the Bears leaving \
         P0's graveyard in the same event that exiles the Mentor",
    );
    assert_eq!(
        offered,
        vec![library_before[0]],
        "the scry prompt offers exactly the top card of P0's library (CR 701.22a)"
    );
    assert_eq!(
        runner.state().objects[&mentor].zone,
        Zone::Exile,
        "the trigger resolves after its own source has left the battlefield"
    );

    let answer = runner
        .act(GameAction::SelectCards {
            cards: offered.clone(),
        })
        .expect("the scry choice must be accepted");
    events.extend(answer.events);
    settle_at_priority(&mut runner, P0, "Shadows' Verdict", &mut events);

    let state = runner.state();
    // CR 701.13a: both printed legs exiled their matching objects.
    assert_eq!(state.objects[&mentor].zone, Zone::Exile);
    assert_eq!(state.objects[&bears].zone, Zone::Exile);
    assert_eq!(sorted(exile_of(state)), sorted(vec![mentor, bears]));
    assert!(battlefield_of(state, P0).is_empty());
    assert!(battlefield_of(state, P1).is_empty());
    assert_eq!(sorted(graveyard_of(state, P0)), vec![verdict]);
    assert!(graveyard_of(state, P1).is_empty());
    let library_after: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library_after, library_before,
        "a keep-on-top scry leaves the library order unchanged"
    );

    // CR 701.22a: exactly one completed Scry resolution.
    assert_eq!(
        scry_resolutions_from(&events, mentor),
        1,
        "one scry for the one graveyard-leaving event"
    );
    // CR 608.2f: both populations moved in ONE `ChangeZoneAll` resolution — the
    // Mentor's and the Bears' moves both precede the single `EffectResolved`
    // the exile publishes (a chained second effect would publish two).
    let resolutions = change_zone_all_resolutions_from(&events, verdict);
    assert_eq!(
        resolutions.len(),
        1,
        "the two populations are one ChangeZoneAll instruction, not two"
    );
    let mentor_move = zone_change_event_index(&events, mentor, Zone::Battlefield, Zone::Exile)
        .expect("the Mentor's battlefield-to-exile move is in the action's events");
    let bears_move = zone_change_event_index(&events, bears, Zone::Graveyard, Zone::Exile)
        .expect("the Bears' graveyard-to-exile move is in the action's events");
    assert!(
        mentor_move < resolutions[0] && bears_move < resolutions[0],
        "both moves belong to the one exile resolution"
    );
}

/// Hostile control for the witness above: the trigger names YOUR graveyard, so
/// the same board with the Bears in P1's graveyard must not prompt P0's Mentor.
/// Reach guards: both the Mentor (P0's battlefield) and the Bears (P1's
/// graveyard) are exiled, so both printed legs resolved; the paired positive in
/// `shadows_verdict_exiling_the_observer_and_a_graveyard_card_triggers_one_scry`
/// proves the same card and setup does trigger for P0's graveyard, so this
/// negative is not vacuous.
#[test]
fn shadows_verdict_exiling_the_observer_does_not_trigger_for_an_opponents_graveyard() {
    let (mut runner, verdict, mentor, bears, _library_before) = mentor_verdict_game(P1);
    drop(runner.cast(verdict).commit());

    let mut events: Vec<GameEvent> = Vec::new();
    let offered = resolve_to_scry_prompt(&mut runner, P0, &mut events);
    assert!(
        offered.is_none(),
        "P0's Mentor must not trigger for cards leaving P1's graveyard, got {offered:?}"
    );
    settle_at_priority(
        &mut runner,
        P0,
        "Shadows' Verdict hostile control",
        &mut events,
    );

    let state = runner.state();
    assert_eq!(state.objects[&mentor].zone, Zone::Exile);
    assert_eq!(state.objects[&bears].zone, Zone::Exile);
    assert_eq!(sorted(exile_of(state)), sorted(vec![mentor, bears]));
    assert!(battlefield_of(state, P0).is_empty());
    assert_eq!(sorted(graveyard_of(state, P0)), vec![verdict]);
    assert!(graveyard_of(state, P1).is_empty());
    assert_eq!(
        scry_resolutions_from(&events, mentor),
        0,
        "P1's graveyard is not the Mentor's graveyard"
    );
}

/// The same simultaneous mass exile carried by Once More with Feeling ("Exile
/// all permanents and all cards from all graveyards"): the Mentor is one of the
/// exiled permanents and the Grizzly Bears one of the exiled graveyard cards,
/// so the one leaves-your-graveyard event triggers exactly one scry
/// (CR 608.2f + CR 603.10a). The rest of the printed card — hand into library,
/// draw seven, life becomes 10 — runs first as its own instructions and is
/// asserted only as the reach guard proving the whole card resolved.
#[test]
fn once_more_with_feeling_exiling_the_observer_and_a_graveyard_card_triggers_one_scry() {
    let db = load_db().expect("the committed integration card fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let feeling = scenario.add_real_card(P0, "Once More with Feeling", Zone::Hand, db);
    let mentor = scenario.add_real_card(P0, "Stonebound Mentor", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    // Each player draws seven: both libraries must hold them or the draw ends the game.
    for n in 0..9 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {n}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {n}"));
    }
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|i| ManaUnit::new(ManaType::White, ObjectId(9_200 + i), false, vec![]))
            .collect(),
    );
    let mut runner: GameRunner = scenario.build();
    drop(runner.cast(feeling).commit());

    let mut events: Vec<GameEvent> = Vec::new();
    let offered = resolve_to_scry_prompt(&mut runner, P0, &mut events).expect(
        "CR 603.10a + CR 608.2f: Stonebound Mentor must trigger once for the Bears leaving \
         P0's graveyard in the same event that exiles the Mentor",
    );
    {
        let state = runner.state();
        // Reach guards: the whole printed card resolved before the trigger.
        assert_eq!(state.players[P0.0 as usize].hand.len(), 7, "drew seven");
        assert_eq!(state.players[P0.0 as usize].life, 10);
        assert_eq!(state.objects[&mentor].zone, Zone::Exile);
        assert_eq!(state.objects[&bears].zone, Zone::Exile);
        assert!(battlefield_of(state, P0).is_empty());
        assert!(graveyard_of(state, P0).is_empty());
        assert_eq!(
            offered,
            vec![*state.players[P0.0 as usize]
                .library
                .front()
                .expect("the shuffled library still holds the two undrawn cards")],
            "the scry sees the top card of the freshly shuffled library"
        );
    }

    let answer = runner
        .act(GameAction::SelectCards { cards: offered })
        .expect("the scry choice must be accepted");
    events.extend(answer.events);
    settle_at_priority(&mut runner, P0, "Once More with Feeling", &mut events);

    assert_eq!(
        scry_resolutions_from(&events, mentor),
        1,
        "one scry for the one graveyard-leaving event"
    );
}

// ---------------------------------------------------------------------------
// Decree of Annihilation — one population leg per zone family
// (battlefield permanents / graveyards / hands), resolved as ONE instruction.
// ---------------------------------------------------------------------------

/// Decree of Annihilation is {8}{R}{R}: two red plus eight generic.
fn decree_pool() -> Vec<ManaUnit> {
    let mut kinds = vec![ManaType::Red; 2];
    kinds.extend([ManaType::Colorless; 8]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9_300 + i as u64), false, vec![]))
        .collect()
}

/// One player's staged population for Decree of Annihilation, split by the zone
/// the printed instruction must move it out of.
struct DecreeBoard {
    artifact: ObjectId,
    creature: ObjectId,
    land: ObjectId,
    /// Named by no leg: the card names artifacts, creatures and lands only, so
    /// this enchantment beside them must stay (CR 205.2a, the type axis).
    enchantment: ObjectId,
    graveyard_card: ObjectId,
    hand_cards: [ObjectId; 2],
}

/// Stage one player's half: an artifact, a creature and a land on the
/// battlefield, an enchantment that no leg names, one creature card in the
/// graveyard, and two cards in hand. Textless fillers — the only rules text in
/// this game is the printed sorcery's.
fn stage_decree_board(scenario: &mut GameScenario, player: PlayerId, tag: &str) -> DecreeBoard {
    let artifact = scenario
        .add_artifact_from_oracle(player, &format!("{tag} Artifact"), "")
        .id();
    let creature = scenario
        .add_creature(player, &format!("{tag} Creature"), 2, 2)
        .id();
    let land = scenario.add_basic_land(player, ManaColor::Green);
    let enchantment = scenario
        .add_enchantment_from_oracle(player, &format!("{tag} Enchantment"), "")
        .id();
    let graveyard_card = scenario
        .add_creature_to_graveyard(player, &format!("{tag} Graveyard Creature"), 2, 2)
        .id();
    let hand_cards = [
        scenario
            .add_creature_to_hand(player, &format!("{tag} Hand Creature A"), 1, 1)
            .id(),
        scenario
            .add_creature_to_hand(player, &format!("{tag} Hand Creature B"), 1, 1)
            .id(),
    ];
    DecreeBoard {
        artifact,
        creature,
        land,
        enchantment,
        graveyard_card,
        hand_cards,
    }
}

/// Decree of Annihilation — Sorcery, {8}{R}{R}: "Exile all artifacts, creatures,
/// and lands from the battlefield, all cards from all graveyards, and all cards
/// from all hands."
///
/// Three legs, three zone families, ONE `ChangeZoneAll` (CR 608.2f).
/// Discriminators, per player: the artifact, creature and land leave the
/// battlefield while the enchantment beside them stays (type axis); the
/// graveyard creature card and both hand cards leave their zones (zone axis);
/// and the resolved sorcery ends in its owner's graveyard without being exiled
/// by its own instruction (CR 608.2n — it was on the stack, not in a graveyard,
/// when the instruction ran).
#[test]
fn decree_of_annihilation_exiles_battlefield_graveyards_and_hands_in_one_resolution() {
    let db = load_db().expect("the committed integration card fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decree = scenario.add_real_card(P0, "Decree of Annihilation", Zone::Hand, db);
    let p0 = stage_decree_board(&mut scenario, P0, "P0");
    let p1 = stage_decree_board(&mut scenario, P1, "P1");
    scenario.with_mana_pool(P0, decree_pool());
    let mut runner: GameRunner = scenario.build();

    let outcome = runner.cast(decree).resolve();
    let state = outcome.state();
    let events = outcome.events();

    // CR 701.13a reach guards: all three legs moved their whole population, for
    // both players, so the enchantment negative below cannot pass vacuously.
    for board in [&p0, &p1] {
        outcome.assert_zone(&[board.artifact, board.creature, board.land], Zone::Exile);
        outcome.assert_zone(&[board.graveyard_card], Zone::Exile);
        outcome.assert_zone(&board.hand_cards, Zone::Exile);
    }

    // Exact battlefield membership: only the enchantment survives, per player.
    assert_eq!(
        sorted(battlefield_of(state, P0)),
        sorted(vec![p0.enchantment]),
        "P0 battlefield: the enchantment is the only permanent Decree does not name"
    );
    assert_eq!(
        sorted(battlefield_of(state, P1)),
        sorted(vec![p1.enchantment]),
        "P1 battlefield: the enchantment is the only permanent Decree does not name"
    );

    // Exact graveyard membership: every staged graveyard card is gone, and the
    // only card left there is the resolved sorcery itself (CR 608.2n).
    assert_eq!(
        sorted(graveyard_of(state, P0)),
        sorted(vec![decree]),
        "P0 graveyard holds only the resolved Decree"
    );
    assert!(
        graveyard_of(state, P1).is_empty(),
        "P1's graveyard card was exiled"
    );
    for player in [P0, P1] {
        assert!(
            state.players[player.0 as usize].hand.is_empty(),
            "{player:?}'s hand was exiled in full"
        );
    }

    // Exact exile membership: three permanents + one graveyard card + two hand
    // cards per player, and nothing else — the resolved Decree is not among them.
    let mut expected_exile: Vec<ObjectId> = Vec::new();
    for board in [&p0, &p1] {
        expected_exile.extend([
            board.artifact,
            board.creature,
            board.land,
            board.graveyard_card,
            board.hand_cards[0],
            board.hand_cards[1],
        ]);
    }
    assert_eq!(
        sorted(exile_of(state)),
        sorted(expected_exile),
        "nothing besides the three named populations is exiled"
    );
    assert!(
        !exile_of(state).contains(&decree),
        "CR 608.2n: the resolving sorcery is in the graveyard, not exiled by its own instruction"
    );

    // CR 608.2f: three populations, one action -> one `ChangeZoneAll` resolution.
    let resolutions = change_zone_all_resolutions_from(events, decree);
    assert_eq!(
        resolutions.len(),
        1,
        "the battlefield, graveyard and hand populations are one ChangeZoneAll instruction"
    );
    // Each zone leg's move precedes that single completion event: a chained
    // execution (one effect per leg) would publish one completion per leg.
    for (what, object, from) in [
        ("battlefield artifact", p0.artifact, Zone::Battlefield),
        ("graveyard card", p0.graveyard_card, Zone::Graveyard),
        ("hand card", p0.hand_cards[0], Zone::Hand),
    ] {
        let index = zone_change_event_index(events, object, from, Zone::Exile)
            .unwrap_or_else(|| panic!("{what}: no {from:?} -> Exile move recorded"));
        assert!(
            index < resolutions[0],
            "{what} moved inside the one exile resolution"
        );
    }
    // CR 608.2n: the spell's own stack -> graveyard move is the final part of
    // the resolution, a public event after the instruction that exiled the three
    // populations.
    let decree_to_graveyard = zone_change_event_index(events, decree, Zone::Stack, Zone::Graveyard)
        .expect("CR 608.2n: the resolving sorcery moves from the stack to its owner's graveyard");
    assert!(
        decree_to_graveyard > resolutions[0],
        "the spell leaves the stack after its instruction resolved"
    );

    assert_settled(&outcome, P0, "Decree of Annihilation");
}

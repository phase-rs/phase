//! Regression for issue #3316: Stinging Study must correctly resolve X
//! as the mana value of a commander you own on the battlefield or in the command zone.
//!
//! https://github.com/phase-rs/phase/issues/3316
//!
//! Casts real Stinging Study card data with a known-mana-value commander in the
//! command zone and asserts that the controller draws X cards and loses X life.
//! This exercises the full parser → cast → resolve pipeline so a regression in
//! any of those layers will cause this test to fail.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    static DB: std::sync::OnceLock<Option<CardDatabase>> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_3316_cards.json");
        CardDatabase::from_export(&path)
            .expect("issue_3316_cards.json fixture must load")
            .into()
    })
    .as_ref()
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Stinging Study draws X cards and loses X life where X = commander's mana value.
///
/// This test verifies the full cast→resolve pipeline: the real Stinging Study
/// ability (loaded from a committed fixture with `CommanderManaValue` quantities)
/// is cast with a 4-mana commander in the command zone and the outcomes are
/// asserted.
#[test]
fn stinging_study_cast_draws_cards_and_loses_life_equal_to_commander_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 4-mana commander in the command zone — Stinging Study resolves X = 4.
    let commander_id = scenario
        .add_creature(P0, "Test Commander", 3, 3)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_commander(commander_id);

    // Real Stinging Study ({4}{B}) from the fixture — uses CommanderManaValue.
    let stinging_study = scenario.add_real_card(P0, "Stinging Study", Zone::Hand, db);

    // Stock P0's library so there are cards to draw (X=4).
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Filler Card");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Provide {4}{B} to pay the casting cost.
    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
        ],
    );

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len() as i32;

    let outcome = runner.cast(stinging_study).resolve();

    // After casting: Stinging Study leaves hand (-1) and X=4 cards drawn (+4).
    // Net change = +3. X = commander's mana value = 4.
    let hand_after = outcome.state().players[0].hand.len() as i32;
    assert_eq!(
        hand_after - hand_before,
        3, // net: -1 (spell cast) + 4 (drawn) = +3
        "Stinging Study should net draw X-1=3 cards (spell leaves hand + draw X=4)"
    );

    let life_after = outcome.state().players[0].life;
    assert_eq!(
        life_before - life_after,
        4,
        "Stinging Study should lose X=4 life (commander's mana value)"
    );
}

/// F2-sting1: Stinging Study chooses ONCE for draw AND lose, and the choice
/// survives the Draw postlude (the carrier-survival proof).
///
/// Two owned commanders (MV 2 on the battlefield, MV 4 in the command zone);
/// the library is packed hostile with MV-9 cards on top, so the drawn cards
/// outrank the choice. Choosing the MV-4 commander draws 4 and loses 4 — a
/// bare carrier would union the drawn MV-9s into the choice set and lose 9.
/// No second prompt may appear (a re-park would offer one).
///
/// Revert-to-red: no prompt is offered (the `prompts_seen` assertion fails);
/// against a bare carrier the life delta is 9, not 4.
#[test]
fn stinging_study_chooses_once_for_draw_and_lose() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bf_commander = scenario
        .add_creature(P0, "Battlefield Commander", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .commander()
        .id();
    let cz_commander = scenario
        .add_creature(P0, "Command-Zone Commander", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_commander(cz_commander);

    let stinging_study = scenario.add_real_card(P0, "Stinging Study", Zone::Hand, db);

    // Fillers first, then the hostile MV-9 cards on top (each call places on
    // top, so the last-added hostile is drawn first).
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Filler Card");
    }
    let mut hostile = Vec::new();
    for i in 0..6 {
        hostile.push(scenario.add_card_to_library_top(P0, &format!("Hostile Nine {i}")));
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    for id in &hostile {
        runner.state_mut().objects.get_mut(id).unwrap().mana_cost = ManaCost::generic(9);
    }

    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
        ],
    );

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len() as i32;

    runner.cast(stinging_study).commit();
    let mut prompts_seen = 0;
    for _ in 0..16 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                prompts_seen += 1;
                assert_eq!(
                    prompts_seen, 1,
                    "Stinging Study must choose exactly ONCE for draw and lose"
                );
                assert_eq!(player, P0, "the owner chooses their commander");
                assert_eq!(cards.len(), 2, "both in-zone commanders offered: {cards:?}");
                assert!(
                    cards.contains(&bf_commander),
                    "battlefield offered: {cards:?}"
                );
                assert!(
                    cards.contains(&cz_commander),
                    "command zone offered: {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cz_commander],
                    })
                    .expect("answer the commander choice");
            }
            other => panic!("unexpected prompt resolving Stinging Study: {other:?}"),
        }
    }
    assert_eq!(
        prompts_seen, 1,
        "the commander choice prompt was never offered"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Stinging Study must resolve back to priority with an empty stack"
    );

    // X = 4 (the chosen command zone commander): draw 4, lose 4.
    let hand_after = runner.state().players[0].hand.len() as i32;
    assert_eq!(
        hand_after - hand_before,
        3, // net: -1 (spell cast) + 4 (drawn) = +3
        "Stinging Study should net draw X-1=3 cards for the chosen X=4"
    );
    let life_after = runner.state().players[0].life;
    assert_eq!(
        life_before - life_after,
        4,
        "Stinging Study should lose the chosen X=4 life, not the drawn MV-9s"
    );
    // Hostility was live: the drawn cards really were the MV-9s (all four
    // draws came off the hostile top), so the life delta proves the
    // cause-stamped read, not Max(all).
    let drawn_hostile = hostile
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Hand)
        .count();
    assert_eq!(
        drawn_hostile, 4,
        "the four draws must have come off the hostile MV-9 top"
    );
}

/// F2-stingZ: zone limits preserved — the hand commander is excluded, the
/// singleton stays silent (guard against over-widening to ALL).
///
/// Passes both pre- and post-fix; fails if the zones over-widen (a prompt
/// appears, or X reads the hand commander).
#[test]
fn stinging_study_ignores_hand_commander() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cz_commander = scenario
        .add_creature(P0, "Command-Zone Commander", 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_commander(cz_commander);
    let hand_commander = scenario
        .add_creature_to_hand(P0, "Hand Commander", 7, 7)
        .with_mana_cost(ManaCost::generic(7))
        .commander()
        .id();

    let stinging_study = scenario.add_real_card(P0, "Stinging Study", Zone::Hand, db);

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Filler Card");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
        ],
    );

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len() as i32;

    let outcome = runner.cast(stinging_study).resolve();
    // No prompt: the hand commander is outside the zone-qualified population.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no prompt may appear for a single in-zone commander, got {:?}",
        outcome.final_waiting_for()
    );

    // X = 5 (the command-zone commander), not 7 (the hand commander).
    let hand_after = outcome.state().players[0].hand.len() as i32;
    assert_eq!(
        hand_after - hand_before,
        4, // net: -1 (spell cast) + 5 (drawn) = +4
        "Stinging Study should net draw X-1=4 cards for X=5"
    );
    let life_after = outcome.state().players[0].life;
    assert_eq!(
        life_before - life_after,
        5,
        "Stinging Study should lose X=5 life (command-zone MV), not the hand MV 7"
    );
    // Paired reach-guard: the hand commander was present all along, so its
    // exclusion proves the zone limit rather than absence.
    assert_eq!(
        outcome.state().objects[&hand_commander].zone,
        Zone::Hand,
        "the hand commander must still be in hand (present but excluded)"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "Stinging Study must resolve with an empty stack"
    );
}

/// F2-zero (Stinging half): no owned commander → X = 0, no prompt, never
/// stuck. The opponent HAS a commander (ownership hostile): it must not
/// count.
#[test]
fn stinging_study_with_no_commander_draws_nothing_silently() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let opponent_commander = scenario
        .add_creature(P1, "Opponent Commander", 6, 6)
        .with_mana_cost(ManaCost::generic(6))
        .commander()
        .id();

    let stinging_study = scenario.add_real_card(P0, "Stinging Study", Zone::Hand, db);

    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Filler Card");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
        ],
    );

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len() as i32;

    let outcome = runner.cast(stinging_study).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no prompt may appear with no owned commander, got {:?}",
        outcome.final_waiting_for()
    );

    let hand_after = outcome.state().players[0].hand.len() as i32;
    assert_eq!(
        hand_after - hand_before,
        -1, // net: -1 (spell cast) + 0 (drawn) = -1
        "Stinging Study with no commander should draw nothing"
    );
    let life_after = outcome.state().players[0].life;
    assert_eq!(
        life_before - life_after,
        0,
        "Stinging Study with no commander should lose no life"
    );
    assert_eq!(
        outcome.state().objects[&opponent_commander].zone,
        Zone::Battlefield,
        "the opponent's commander must not count toward the read"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "Stinging Study must resolve with an empty stack"
    );
}

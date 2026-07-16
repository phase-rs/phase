//! Runtime pipeline regression for issue #5656 — Archmage Ascension.
//!
//! Archmage Ascension's gated draw replacement:
//!   "As long as this enchantment has six or more quest counters on it, if you
//!    would draw a card, you may instead search your library for a card, put
//!    that card into your hand, then shuffle."
//!
//! The parser lifts the "As long as <source has six or more quest counters>"
//! clause into a typed `ReplacementCondition::OnlyIfQuantity` gate over the
//! source's quest counters (CR 614.1a), and the "you may instead" makes the
//! substitution optional (CR 614.6 — the accept branch replaces the draw; it
//! does not supplement it).
//!
//! The parser-level test in `oracle_replacement.rs` proves the
//! `ReplacementDefinition` shape. These tests drive the real engine pipeline
//! through `GameAction`s to prove runtime behavior the parse assertion cannot:
//!
//!   1. Fewer than six quest counters → the `OnlyIfQuantity` gate fails, the
//!      draw is untouched, and no replacement prompt surfaces.
//!   2. Six quest counters → the replacement is offered as an Accept/Decline
//!      optional choice.
//!   3. Accepting searches the library and tutors the chosen card into hand,
//!      fully replacing the draw (+1 hand — the tutored card, NOT the top of
//!      the library, and NOT +2). This is the load-bearing discriminator: it
//!      fails if the optional continuation is dropped to a mandatory or
//!      always-supplement form.
//!   4. Declining falls through to the normal draw (+1 hand — the top card),
//!      with no search and no shuffle.
//!
//! Removing the `OnlyIfQuantity` gate breaks test (1) (the replacement would
//! fire at five counters); removing the optional continuation breaks tests (3)
//! and (4) (there would be no Accept/Decline prompt).

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn quest_counter() -> CounterType {
    CounterType::Generic("quest".to_string())
}

/// Install Archmage Ascension on `P0`'s battlefield with `quest_counters` quest
/// counters, a deterministic `P0` library (`lib_top_to_bottom[0]` is the top of
/// the library — `add_real_card` `push_back`s and the engine treats
/// `library.front()` as the top), and a padded `P1` library so SBAs stay quiet.
/// Returns the runner and the Archmage Ascension object id.
fn scenario_with_archmage(
    db: &CardDatabase,
    quest_counters: u32,
    lib_top_to_bottom: &[&str],
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archmage = scenario.add_real_card(P0, "Archmage Ascension", Zone::Battlefield, db);
    scenario.with_counter(archmage, quest_counter(), quest_counters);
    for name in lib_top_to_bottom.iter() {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    // P1 needs *some* library so SBAs don't fire.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;
    (runner, archmage)
}

fn issue_single_draw(runner: &mut engine::game::scenario::GameRunner) {
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
}

fn hand_card_names(state: &engine::types::game_state::GameState, player: PlayerId) -> Vec<String> {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

/// CR 614.1a: with only five quest counters the `OnlyIfQuantity` gate fails, so
/// the draw is never a replacement candidate. No prompt surfaces and the top of
/// the library is drawn normally. Fails if the counter gate is removed (the
/// replacement would fire below the six-counter threshold).
#[test]
fn archmage_below_six_counters_leaves_draw_untouched() {
    let Some(db) = load_db() else {
        return;
    };

    let (mut runner, _archmage) = scenario_with_archmage(
        db,
        5,
        &["Grizzly Bears", "Hill Giant", "Plains", "Sol Ring"],
    );

    let hand_before = runner.state().players[0].hand.len();

    issue_single_draw(&mut runner);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "five quest counters must NOT offer Archmage Ascension's replacement; got {:?}",
        runner.state().waiting_for
    );

    runner.advance_until_stack_empty();

    let hand_after_names = hand_card_names(runner.state(), P0);
    assert_eq!(
        hand_after_names.len(),
        hand_before + 1,
        "below the gate the draw proceeds normally (+1); got {hand_after_names:?}"
    );
    assert!(
        hand_after_names.contains(&"Grizzly Bears".to_string()),
        "the natural top-of-library draw (Grizzly Bears) must enter hand; got {hand_after_names:?}"
    );
}

/// CR 614.1a + CR 614.6: with six quest counters the gate opens and the "you
/// may instead" substitute is offered as an optional Accept/Decline choice.
/// Accepting searches the library and tutors the *chosen* card into hand,
/// fully replacing the draw — +1 hand (the tutored Sol Ring, NOT the top-of-
/// library Grizzly Bears, and NOT +2). Fails if the optional continuation is
/// dropped (no Accept/Decline prompt) or if the draw supplements rather than
/// replaces (+2 / top card also drawn).
#[test]
fn archmage_six_counters_accept_tutors_chosen_card_and_replaces_draw() {
    let Some(db) = load_db() else {
        return;
    };

    // Grizzly Bears is on top; Sol Ring is buried. A normal draw would take
    // Grizzly Bears — the tutor must instead fetch the buried Sol Ring.
    let (mut runner, _archmage) = scenario_with_archmage(
        db,
        6,
        &["Grizzly Bears", "Hill Giant", "Plains", "Sol Ring"],
    );

    let hand_before = runner.state().players[0].hand.len();

    issue_single_draw(&mut runner);

    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "six quest counters must offer Archmage Ascension's optional replacement; got {:?}",
            runner.state().waiting_for
        );
    };
    let descriptions: Vec<&str> = candidates.iter().map(|c| c.description.as_str()).collect();
    assert_eq!(
        descriptions,
        vec!["Accept", "Decline"],
        "the gated substitute must surface exactly Accept/Decline"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Archmage Ascension's optional search substitute");

    // The substitute is "search your library for a card" — the engine pauses at
    // the SearchChoice boundary so we can pick the buried Sol Ring.
    let sol_ring = match runner.state().waiting_for.clone() {
        WaitingFor::SearchChoice { cards, .. } => cards
            .iter()
            .find(|&&id| runner.state().objects[&id].name == "Sol Ring")
            .copied()
            .expect("Sol Ring must be a legal search choice"),
        other => panic!("expected SearchChoice after accepting the substitute, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![sol_ring],
        })
        .expect("selecting Sol Ring resolves the tutor substitute");

    runner.advance_until_stack_empty();

    let hand_after_names = hand_card_names(runner.state(), P0);

    // ── Discriminating assertion: +1 (the tutored card), not +2. ────────────
    // If "you may instead" wired a mandatory or always-supplement replacement,
    // the accept branch would also draw the top card (Grizzly Bears), yielding
    // +2. CR 614.6 requires the draw be fully replaced → exactly +1.
    assert_eq!(
        hand_after_names.len(),
        hand_before + 1,
        "accept must yield exactly +1 hand card (the tutored Sol Ring), not +2 — \
         the draw is fully replaced; got {hand_after_names:?}"
    );
    assert!(
        hand_after_names.contains(&"Sol Ring".to_string()),
        "the tutored Sol Ring must be in hand; got {hand_after_names:?}"
    );
    assert!(
        !hand_after_names.contains(&"Grizzly Bears".to_string()),
        "the top-of-library card (Grizzly Bears) must NOT be drawn — the draw was \
         replaced, not supplemented; got {hand_after_names:?}"
    );
    // The tutored card left the library.
    assert_eq!(
        runner.state().objects[&sol_ring].zone,
        Zone::Hand,
        "Sol Ring must move library → hand"
    );
}

/// CR 614.6: declining the optional substitute falls through to the original
/// draw, which proceeds unmodified — the top-of-library card is drawn (+1) with
/// no search. Fails if the optional continuation is dropped (there would be no
/// Decline branch to take).
#[test]
fn archmage_six_counters_decline_falls_through_to_normal_draw() {
    let Some(db) = load_db() else {
        return;
    };

    let (mut runner, _archmage) = scenario_with_archmage(
        db,
        6,
        &["Grizzly Bears", "Hill Giant", "Plains", "Sol Ring"],
    );

    let hand_before = runner.state().players[0].hand.len();

    issue_single_draw(&mut runner);

    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "six quest counters must offer the optional replacement; got {:?}",
            runner.state().waiting_for
        );
    };
    let decline_idx = candidates
        .iter()
        .position(|c| c.description == "Decline")
        .expect("Decline option must be present for the optional substitute");
    runner
        .act(GameAction::ChooseReplacement { index: decline_idx })
        .expect("decline Archmage Ascension's optional substitute");

    runner.advance_until_stack_empty();

    let hand_after_names = hand_card_names(runner.state(), P0);
    assert_eq!(
        hand_after_names.len(),
        hand_before + 1,
        "decline must result in exactly +1 hand card (the natural draw); got {hand_after_names:?}"
    );
    assert!(
        hand_after_names.contains(&"Grizzly Bears".to_string()),
        "declining draws the top-of-library card (Grizzly Bears); got {hand_after_names:?}"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "decline must NOT trigger a search; got {:?}",
        runner.state().waiting_for
    );
}

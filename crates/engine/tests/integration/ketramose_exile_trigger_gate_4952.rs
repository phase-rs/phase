//! Regression (issue #4952): Ketramose, the New Dawn — the batched
//! "put into exile" trigger must fire through the real game pipeline only for
//! exiles that match BOTH gates its Oracle text imposes, and stay silent
//! otherwise.
//!
//! Oracle (the relevant line):
//! "Whenever one or more cards are put into exile from graveyards and/or the
//!  battlefield during your turn, you draw a card and lose 1 life."
//!
//! Two independent gates the parser now models on the `ChangesZoneAll`
//! trigger and the runtime (`trigger_matchers.rs` origin_zones +
//! `triggers.rs` `OnlyDuringYourTurn`) must jointly enforce:
//!   * origin_zones = {Graveyard, Battlefield} — an exile FROM those zones
//!     fires; an exile from hand does NOT.
//!   * `OnlyDuringYourTurn` — fires on Ketramose's controller's turn only.
//!
//! Ketramose is built from its **real Oracle text** (not the pre-baked card-DB
//! rules) so these tests exercise the parser → `TriggerDefinition` → runtime
//! path end to end. The exile is caused by real spells resolving through the
//! stack, producing genuine zone-change events. The witness is Ketramose's
//! distinctive "lose 1 life": only its trigger drains its controller's life, so
//! a 1-point drop (or the absence of one) cleanly reports whether it fired.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// The exact printed Oracle text of Ketramose, the New Dawn.
const KETRAMOSE_ORACLE: &str = "Menace, lifelink, indestructible\n\
Ketramose can't attack or block unless there are seven or more cards in exile.\n\
Whenever one or more cards are put into exile from graveyards and/or the \
battlefield during your turn, you draw a card and lose 1 life.";

/// A floating mana pool of the requested colors (one unit each).
fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&c| ManaUnit::new(c, ObjectId(0), false, vec![]))
        .collect()
}

/// Add Ketramose to `P0`'s battlefield from its real Oracle text so the
/// trigger under test comes from the parser, not the card DB's structured rules.
fn add_ketramose(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Ketramose, the New Dawn", 4, 4)
        .from_oracle_text(KETRAMOSE_ORACLE)
        .id()
}

/// CR 603.2 + origin-zone gate: exiling a permanent from the battlefield on
/// Ketramose's controller's turn fires the trigger — its controller draws and
/// loses exactly 1 life.
#[test]
fn battlefield_exile_on_own_turn_fires_draw_and_lose_life() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    add_ketramose(&mut scenario);
    let unmake = scenario.add_real_card(P0, "Unmake", Zone::Hand, db);
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    // Draw fodder so "draw a card" does not deck P0 out.
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::White, ManaType::White, ManaType::White]),
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let life_before = runner.state().players[0].life;
    let outcome = runner.cast(unmake).target_object(victim).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&victim].zone,
        Zone::Exile,
        "Unmake must exile the targeted creature from the battlefield"
    );
    assert_eq!(
        state.players[0].life,
        life_before - 1,
        "battlefield exile on P0's turn must fire Ketramose (P0 loses exactly 1 life)"
    );
}

/// origin-zone gate (graveyard branch): exiling a card from a graveyard on
/// Ketramose's controller's turn also fires the trigger.
#[test]
fn graveyard_exile_on_own_turn_fires_draw_and_lose_life() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    add_ketramose(&mut scenario);
    let cremate = scenario.add_real_card(P0, "Cremate", Zone::Hand, db);
    let graveyard_card = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.with_mana_pool(P0, pool(&[ManaType::Black]));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let life_before = runner.state().players[0].life;
    let outcome = runner.cast(cremate).target_object(graveyard_card).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&graveyard_card].zone,
        Zone::Exile,
        "Cremate must exile the targeted card from the graveyard"
    );
    assert_eq!(
        state.players[0].life,
        life_before - 1,
        "graveyard exile on P0's turn must fire Ketramose (P0 loses exactly 1 life)"
    );
}

/// `OnlyDuringYourTurn` gate: the very same battlefield exile, but on the
/// OPPONENT's turn, must NOT fire Ketramose — P0's life is untouched.
#[test]
fn battlefield_exile_on_opponents_turn_does_not_fire() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    add_ketramose(&mut scenario);
    // P1 owns and casts the exile spell on P1's own turn.
    let unmake = scenario.add_real_card(P1, "Unmake", Zone::Hand, db);
    let victim = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P1,
        pool(&[ManaType::White, ManaType::White, ManaType::White]),
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    {
        let st = runner.state_mut();
        st.active_player = P1;
        st.phase = Phase::PreCombatMain;
        st.priority_player = P1;
        st.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let life_before = runner.state().players[0].life;
    let outcome = runner.cast(unmake).target_object(victim).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&victim].zone,
        Zone::Exile,
        "the exile still happens — this is a real zone-change event"
    );
    assert_eq!(
        state.players[0].life, life_before,
        "an exile on the OPPONENT's turn must not fire Ketramose (P0's life is unchanged)"
    );
}

/// origin-zone gate (exclusion): exiling a card from a HAND — a zone NOT in
/// {Graveyard, Battlefield} — on P0's own turn must NOT fire Ketramose.
#[test]
fn hand_exile_on_own_turn_does_not_fire() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    add_ketramose(&mut scenario);
    let castigate = scenario.add_real_card(P0, "Castigate", Zone::Hand, db);
    // A nonland card in P1's hand for Castigate to reveal-and-exile.
    let hand_card = scenario.add_real_card(P1, "Grizzly Bears", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&[ManaType::White, ManaType::Black]));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let life_before = runner.state().players[0].life;
    // Castigate reveals P1's hand and stops on `RevealChoice` for the caster to
    // pick the nonland card to exile; answer it with the declared card, then
    // let the exile (and any triggers it would fire) resolve.
    runner.cast(castigate).target_player(P1).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![hand_card],
        })
        .expect("P0 choosing the nonland card to exile must be accepted");
    runner.advance_until_stack_empty();
    let state = runner.state();

    assert_eq!(
        state.objects[&hand_card].zone,
        Zone::Exile,
        "Castigate must exile the chosen card out of P1's hand"
    );
    assert_eq!(
        state.players[0].life, life_before,
        "an exile FROM HAND (not a graveyard/battlefield) must not fire Ketramose"
    );
}

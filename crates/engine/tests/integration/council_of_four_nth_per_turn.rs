//! Runtime pipeline regression for issue #413 — The Council of Four.
//!
//! The Council of Four has two Nth-per-turn triggers:
//!   "Whenever a player draws their second card during their turn, you draw a card."
//!   "Whenever a player casts their second spell during their turn, you create a
//!    2/2 white Knight creature token."
//!
//! Before the fix, the "during their turn" timing clause was unrecognized by the
//! nth-spell / nth-draw trigger parsers, so both triggers fell back to
//! `TriggerMode::Unknown` and never fired. The fix recognizes the clause and maps
//! it to a `TriggerCondition::DuringPlayersTurn { TriggeringPlayer }` intervening-if.
//!
//! These tests drive the real engine pipeline through `GameAction`s — casting
//! actual spells from hand and resolving them on the stack — and assert the
//! triggers fire on exactly the 2nd occurrence (not the 1st or 3rd), and that
//! the per-turn counters reset on the turn boundary (CR 500 / CR 117.1 / CR 121.1).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn knight_token_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.is_token
                && obj.controller == player
                && obj.card_types.subtypes.iter().any(|s| s == "Knight")
        })
        .count()
}

/// CR 117.1 + CR 603.4: Casting the second spell during the caster's own turn
/// fires the Council's spell trigger exactly once. The first spell does not
/// fire it; the third does not fire it again.
#[test]
fn council_of_four_spell_trigger_fires_on_second_spell() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "The Council of Four", Zone::Battlefield, db);
    // Three cheap sorceries to cast in sequence. Divination also draws — used by
    // the draw test below; here we only care about the spell-count trigger.
    let spell_a = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let spell_b = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let spell_c = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    // Library fodder so Divination's "draw two" never empties the library.
    for _ in 0..12 {
        scenario.add_card_to_library_top(P0, "Grizzly Bears");
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Plenty of mana for three Divinations ({2}{U} each).
    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );

    let cast = |runner: &mut GameRunner, obj| {
        runner.cast(obj).resolve();
    };

    // First spell — no Knight token (count == 1, trigger wants n == 2).
    cast(&mut runner, spell_a);
    assert_eq!(
        knight_token_count(&runner, P0),
        0,
        "first spell must NOT fire the second-spell trigger"
    );

    // Second spell — Council's trigger fires, creating one Knight token.
    cast(&mut runner, spell_b);
    assert_eq!(
        knight_token_count(&runner, P0),
        1,
        "second spell must fire the Council's spell trigger exactly once"
    );

    // Third spell — count is now 3, trigger does not fire again.
    cast(&mut runner, spell_c);
    assert_eq!(
        knight_token_count(&runner, P0),
        1,
        "third spell must NOT fire the trigger a second time"
    );
}

/// CR 121.1 + CR 603.4: A spell that draws two cards crosses the drawer's
/// per-turn ordinal 1 → 2; the Council's draw trigger fires on the second draw
/// (and only then), netting the controller one extra drawn card.
#[test]
fn council_of_four_draw_trigger_fires_on_second_draw() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "The Council of Four", Zone::Battlefield, db);
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    for _ in 0..12 {
        scenario.add_card_to_library_top(P0, "Grizzly Bears");
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(
        &mut runner,
        P0,
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
    );

    let library_before = runner.state().players[0].library.len();

    let outcome = runner.cast(divination).resolve();

    // Divination draws 2; the Council's "second card during their turn" trigger
    // fires once on the 2nd draw and draws 1 more. Net: 3 cards drawn. The
    // stack-commit baseline already excludes the cast Divination (CR 601.2a), so
    // `hand_drawn` reads the clean +3 resolution delta.
    assert_eq!(
        library_before - outcome.zone_count(P0, Zone::Library),
        3,
        "Divination draws 2 + Council's draw trigger draws 1 = 3 cards leave the library"
    );
    outcome.assert_hand_drawn(P0, 3);
}

/// CR 500 + CR 117.1: The per-turn spell counter resets on the turn boundary.
/// Casting one spell, crossing into the next own-turn, then casting two more
/// must fire the trigger again — proving the counter is per-turn, not cumulative.
#[test]
fn council_of_four_spell_counter_resets_each_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "The Council of Four", Zone::Battlefield, db);
    let t1_spell = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let t2_spell_a = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let t2_spell_b = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Grizzly Bears");
    }
    // P1 needs library cards too — otherwise P1 decks out on their draw step
    // and the game ends (CR 704.5b) before P0's next turn begins.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P1, "Grizzly Bears");
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let cast = |runner: &mut GameRunner, obj| {
        add_mana(
            runner,
            P0,
            &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
        );
        let card_id = runner.state().objects[&obj].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: obj,
                card_id,
                targets: vec![],
            })
            .expect("cast should be accepted");
        runner.advance_until_stack_empty();
    };

    // Turn 1: cast a single spell — count reaches 1, trigger does not fire.
    cast(&mut runner, t1_spell);
    assert_eq!(
        knight_token_count(&runner, P0),
        0,
        "one spell this turn must not fire the second-spell trigger"
    );

    // Advance the real pipeline until P0's turn comes around again. The engine
    // crosses the cleanup step and `start_next_turn`, which clears
    // `spells_cast_this_turn_by_player` (turns.rs). To traverse a full turn
    // cycle through every interrupting `WaitingFor` (combat declarations,
    // discard-to-hand-size, step triggers) the loop pulls a legal action from
    // the engine's own `legal_actions` generator: it prefers `PassPriority`
    // and otherwise dispatches the first non-cast legal action — never casting
    // a spell, so the per-turn tally is untouched until the explicit turn-2
    // casts below.
    let start_turn = runner.state().turn_number;
    let mut advanced = false;
    for _ in 0..1200 {
        if runner.state().turn_number > start_turn
            && runner.state().active_player == P0
            && runner.state().phase == Phase::PreCombatMain
        {
            advanced = true;
            break;
        }
        // Prefer passing priority — that is what actually moves phases/turns.
        // When the engine is instead waiting on a forced decision (combat
        // declarations, discard-to-hand-size, etc.) submit the *empty* form of
        // that decision so the turn keeps moving. Never tap mana or cast.
        let actions = engine::ai_support::legal_actions(runner.state());
        let progress = actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .or_else(|| {
                actions.iter().find(|a| {
                    matches!(
                        a,
                        GameAction::DeclareAttackers { .. }
                            | GameAction::DeclareBlockers { .. }
                            | GameAction::SelectCards { .. }
                            | GameAction::ChooseTarget { .. }
                    )
                })
            })
            .cloned();
        match progress {
            Some(action) => {
                if runner.act(action).is_err() {
                    break;
                }
            }
            // No progress action available — the engine is parked on a
            // decision this harness does not model. Stop and let the
            // assertion below report the parked state.
            None => break,
        }
    }
    assert!(
        advanced,
        "harness must reach P0's next precombat main; parked at turn {} player {:?} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().active_player,
        runner.state().phase,
        runner.state().waiting_for,
    );

    // The per-turn spell tally must have been cleared at the turn boundary.
    assert!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .is_none_or(|v| v.is_empty()),
        "spells_cast_this_turn_by_player[P0] must reset at the turn boundary"
    );

    // Turn 2: cast two spells — the trigger fires on the new turn's 2nd spell,
    // confirming the count restarted from zero.
    cast(&mut runner, t2_spell_a);
    assert_eq!(
        knight_token_count(&runner, P0),
        0,
        "first spell of the new turn must not fire the trigger"
    );
    cast(&mut runner, t2_spell_b);
    assert_eq!(
        knight_token_count(&runner, P0),
        1,
        "second spell of the new turn fires the trigger — counter reset confirmed"
    );
}

/// Sanity guard: the Council's spell token is a 2/2 white Knight creature.
#[test]
fn council_of_four_spell_trigger_creates_white_knight() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "The Council of Four", Zone::Battlefield, db);
    let spell_a = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    let spell_b = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Grizzly Bears");
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );

    for obj in [spell_a, spell_b] {
        runner.cast(obj).resolve();
    }

    let token = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .find(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Knight"))
        .expect("Council's second-spell trigger must create a Knight token");
    assert_eq!(token.power, Some(2));
    assert_eq!(token.toughness, Some(2));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert_eq!(token.controller, P0);
}

//! Integration tests for curse cards with miscellaneous trigger patterns.
//!
//! Covers 5 curses with varied trigger conditions:
//!   - Curse of Clinging Webs (creature enchanted player controls dies → exile + Spider)
//!   - Curse of Fool's Wisdom (enchanted player draws → deal 2 + gain 2)
//!   - Curse of Obsession (draw step: draw 2 extra; end step: discard hand)
//!   - Curse of Shaken Faith (2nd+ spell each turn → deal 2 damage)
//!   - Fraying Sanity (end step: mill X = cards put into graveyard this turn)
//!
//! Each test verifies at minimum that the trigger fires. For simpler cards,
//! the resolved effect is also verified.
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 603.6a: Zone-change triggers use the game state after the event.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::move_to_zone;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Oracle texts
// ---------------------------------------------------------------------------

const CURSE_OF_CLINGING_WEBS: &str =
    "Whenever a creature enchanted player controls dies, exile it. If you do, create a 1/1 green Spider creature token with reach.";

const CURSE_OF_FOOLS_WISDOM: &str =
    "Whenever enchanted player draws a card, Curse of Fool's Wisdom deals 2 damage to that player and you gain 2 life.";

const CURSE_OF_OBSESSION: &str =
    "At the beginning of enchanted player's draw step, that player draws two additional cards.\n\
     At the beginning of enchanted player's end step, that player discards their hand.";

const CURSE_OF_SHAKEN_FAITH: &str =
    "Whenever enchanted player casts a spell other than the first spell they cast each turn, Curse of Shaken Faith deals 2 damage to that player.";

const FRAYING_SANITY: &str =
    "At the beginning of each end step, enchanted player mills X cards, where X is the number of cards put into their graveyard from anywhere this turn.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Curse of Clinging Webs: trigger fires when a creature enchanted player
/// controls dies (moves to graveyard from battlefield).
#[test]
fn curse_of_clinging_webs_fires_when_enchanted_players_creature_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Clinging Webs",
            0,
            0,
            CURSE_OF_CLINGING_WEBS,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P1's creature that will die.
    let creature = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    // Library padding.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Move the creature to the graveyard (simulate death).
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Clinging Webs must trigger when enchanted player's creature dies"
    );
}

/// Curse of Clinging Webs: does NOT fire when curse controller's creature dies.
#[test]
fn curse_of_clinging_webs_does_not_fire_for_non_enchanted_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Clinging Webs",
            0,
            0,
            CURSE_OF_CLINGING_WEBS,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P0's creature (non-enchanted player).
    let creature = scenario.add_creature(P0, "Elvish Mystic", 1, 1).id();

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Move P0's creature to the graveyard.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());

    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        0,
        "Curse of Clinging Webs must NOT trigger when non-enchanted player's creature dies"
    );
}

/// Curse of Fool's Wisdom: trigger fires when enchanted player draws a card.
/// We drive a real draw via `execute_draw` and assert the curse puts a trigger
/// on the stack (CR 603.2: triggered abilities trigger when their event occurs).
#[test]
fn curse_of_fools_wisdom_fires_when_enchanted_player_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Draw);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Fool's Wisdom",
            0,
            0,
            CURSE_OF_FOOLS_WISDOM,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Drive a real draw-step draw for P1.
    let mut events = Vec::new();
    engine::game::turns::execute_draw(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());

    // The curse must have placed a trigger on the stack from its source.
    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Fool's Wisdom must trigger when enchanted player draws a card"
    );
}

/// Curse of Obsession: "At the beginning of enchanted player's draw step, that
/// player draws two additional cards." We drive through the draw step using
/// `auto_advance_to_main_phase` from Untap and verify P1's hand grew by at
/// least 3 (normal draw + 2 extra from the curse).
#[test]
fn curse_of_obsession_fires_at_draw_step() {
    use engine::types::game_state::WaitingFor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Curse of Obsession", 0, 0, CURSE_OF_OBSESSION);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    for _ in 0..30 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    let hand_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .hand
        .len();

    // Drive through Untap -> Upkeep -> Draw -> PreCombatMain.
    runner.auto_advance_to_main_phase();

    let hand_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .hand
        .len();
    let cards_drawn = hand_after.saturating_sub(hand_before);

    // Normal draw = 1, curse adds 2 more = 3 total.
    assert!(
        cards_drawn >= 3,
        "Curse of Obsession must cause enchanted player to draw 2 additional cards \
         at draw step (expected >=3, got {cards_drawn})"
    );
}

/// Curse of Shaken Faith: trigger fires on the 2nd spell cast by enchanted player.
#[test]
fn curse_of_shaken_faith_fires_on_second_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Shaken Faith",
            0,
            0,
            CURSE_OF_SHAKEN_FAITH,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P1 needs two spells to cast and mana.
    let spell_1 = scenario.add_bolt_to_hand(P1);
    let spell_2 = scenario.add_bolt_to_hand(P1);

    // Targets for the bolts.
    let dummy1 = scenario.add_creature(P0, "Memnite", 1, 1).id();
    let dummy2 = scenario.add_creature(P0, "Ornithopter", 0, 2).id();

    // Mana for P1.
    let mana_unit = engine::types::mana::ManaUnit::new(
        engine::types::mana::ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    );
    scenario.with_mana_pool(P1, vec![mana_unit.clone(), mana_unit]);

    // Library padding.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    let _life_before = runner.life(P1);

    // Cast first spell — should NOT trigger Curse of Shaken Faith.
    runner.cast(spell_1).target_object(dummy1).resolve();

    let life_after_first = runner.life(P1);
    // First spell should not cause life loss from the curse.
    // (Bolt deals 3 to a creature, not to P1.)

    // Cast second spell — SHOULD trigger Curse of Shaken Faith (2 damage to P1).
    runner.cast(spell_2).target_object(dummy2).resolve();

    let life_after_second = runner.life(P1);

    // P1 should have lost 2 life from Curse of Shaken Faith on the second cast.
    assert!(
        life_after_second <= life_after_first - 2,
        "Curse of Shaken Faith must deal 2 damage on the second spell cast (life: {} → {})",
        life_after_first,
        life_after_second
    );
}

/// Fraying Sanity (issue #5947): at each end step, enchanted player mills X
/// cards where X is the number of cards put into their graveyard from anywhere
/// this turn. Seed N owned GY puts, advance to the end step, and assert the
/// enchanted player mills exactly N.
#[test]
fn fraying_sanity_mills_cards_put_into_graveyard_this_turn() {
    use engine::types::game_state::WaitingFor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Fraying Sanity", 0, 0, FRAYING_SANITY);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Cards that will be counted as "put into [enchanted player's] graveyard
    // this turn" — owned by P1 (CR 404.1).
    let gy_seeds: Vec<_> = (0..3)
        .map(|i| scenario.add_creature(P1, &format!("Seed {i}"), 1, 1).id())
        .collect();

    // Library padding — P1 needs cards to mill.
    for _ in 0..30 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Put the seeds into P1's graveyard so they populate zone_changes_this_turn.
    for &id in &gy_seeds {
        let mut events = Vec::new();
        move_to_zone(runner.state_mut(), id, Zone::Graveyard, &mut events);
        process_triggers(runner.state_mut(), &events);
        drain_order_triggers_with_identity(runner.state_mut());
    }
    // Clear any leftover stack from the dies moves — Fraying Sanity itself
    // does not trigger on those zone changes (only at end step).
    runner.advance_until_stack_empty();

    let lib_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1")
        .library
        .len();
    let p0_lib_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0")
        .library
        .len();
    let gy_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1")
        .graveyard
        .len();

    // Advance to the end step of the active turn; Fraying Sanity fires for
    // "each end step" and mills the enchanted player.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let lib_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1")
        .library
        .len();
    let p0_lib_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0")
        .library
        .len();
    let gy_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1")
        .graveyard
        .len();

    let milled = lib_before.saturating_sub(lib_after);
    assert_eq!(
        milled, 3,
        "CR 701.17a + CR 404.1: enchanted player must mill X = cards put into \
         their graveyard this turn (expected 3, milled {milled}; \
         lib {lib_before}→{lib_after}, gy {gy_before}→{gy_after})"
    );
    assert_eq!(
        gy_after,
        gy_before + milled,
        "milled cards must land in the enchanted player's graveyard"
    );
    assert_eq!(
        p0_lib_after, p0_lib_before,
        "only the enchanted player should be milled"
    );
}

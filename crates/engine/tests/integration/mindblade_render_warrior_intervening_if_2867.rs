//! Mindblade Render — "Whenever your opponents are dealt combat damage, if any
//! of that damage was dealt by a Warrior, you draw a card and you lose 1 life."
//!
//! Regression for issue #2867: the intervening-if "if any of that damage was
//! dealt by a Warrior" (CR 603.4) was dropped during trigger parsing, so the
//! ability drew a card and lost 1 life on ANY combat damage to opponents,
//! regardless of the damaging creature's types. Per CR 120.1 (the object that
//! deals damage is the source of that damage) and CR 603.4 (intervening-if), the
//! trigger now carries `TriggerCondition::EventDamageSourceMatchesFilter` over a
//! Warrior filter, evaluated against the combat-damage event's sources.
//!
//! Discriminating end-to-end: in one game a Warrior attacker deals the combat
//! damage (the trigger fires — P0 draws and loses 1 life); in the other a
//! non-Warrior attacker deals it (the intervening-if fails — P0 neither draws
//! nor loses life).

use super::rules::run_combat;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const ORACLE: &str = "Whenever your opponents are dealt combat damage, if any of \
    that damage was dealt by a Warrior, you draw a card and you lose 1 life.";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player must exist")
}

/// CR 603.4 + CR 120.1: a Warrior dealt the combat damage, so the intervening-if
/// holds — P0 draws a card and loses 1 life.
#[test]
fn mindblade_render_fires_when_a_warrior_deals_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0's library must have a card to draw when the trigger resolves.
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    scenario.add_creature_from_oracle(P0, "Mindblade Render", 2, 2, ORACLE);
    let warrior = scenario
        .add_creature(P0, "Bushi Warrior", 3, 3)
        .with_subtypes(vec!["Human", "Warrior"])
        .id();

    let mut runner = scenario.build();
    let p0_life_before = runner.life(P0);
    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);

    run_combat(&mut runner, vec![warrior], vec![]);
    runner
        .state_mut()
        .objects
        .get_mut(&warrior)
        .expect("attacker must remain present before trigger resolution")
        .card_types
        .subtypes = vec!["Human".to_string()];
    // CR 603.3: the intervening-if trigger went on the stack during the combat
    // damage step; resolve it (the draw/lose-life are mandatory, no prompt).
    // CR 608.2i + CR 608.2h: "that damage was dealt by a Warrior" reads the
    // source's damage-time snapshot, not its live subtypes at resolution.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the Warrior dealt 3 combat damage to P1"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "a Warrior dealt the damage — Mindblade Render's controller draws a card"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before - 1,
        "a Warrior dealt the damage — Mindblade Render's controller loses 1 life"
    );
}

/// CR 603.4 + CR 120.1: a non-Warrior dealt the combat damage, so the
/// intervening-if fails — no draw, no life loss. This is the case the dropped
/// condition got wrong (it drew/lost life unconditionally).
#[test]
fn mindblade_render_silent_when_no_warrior_deals_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C"] {
        scenario.add_card_to_library_top(P0, name);
    }

    scenario.add_creature_from_oracle(P0, "Mindblade Render", 2, 2, ORACLE);
    // A Bear — explicitly NOT a Warrior.
    let bear = scenario
        .add_creature(P0, "Grizzly Bears", 3, 3)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    let p0_life_before = runner.life(P0);
    let p0_hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);

    run_combat(&mut runner, vec![bear], vec![]);
    // CR 603.4: with the intervening-if respected the trigger must not even be
    // on the stack; draining confirms no draw/lose-life sneaks through. Before
    // the fix the dropped condition queued the effect here and P0 drew.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "precondition: the non-Warrior dealt 3 combat damage to P1"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "no Warrior dealt the damage — intervening-if fails, no card drawn"
    );
    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "no Warrior dealt the damage — intervening-if fails, no life lost"
    );
}

//! Regression (issue #425): an activated ability whose *effect* moves the
//! source card out of a non-battlefield zone must be activatable from that
//! zone.
//!
//! CR 113.6m: "An ability whose cost or effect specifies that it moves the
//! object it's on out of a particular zone functions only in that zone."
//!
//! Talon Gates of Madara has `{4}: Put this card from your hand onto the
//! battlefield.` The "from your hand" lives in the *effect* (a self-`ChangeZone`
//! with `origin: Hand`), not the cost. The parser previously derived
//! `activation_zone` only from the cost, so it stayed `None` → the runtime gate
//! (`casting.rs`, `unwrap_or(Zone::Battlefield)`) defaulted to `Battlefield` and
//! rejected the ability because the card sits in `Zone::Hand`.
//!
//! Fix: `activation_zone_from_self_effect` (parser) derives the activation zone
//! from a self-`ChangeZone` effect's `origin`. This test drives the real `apply`
//! pipeline to prove the ability is now activatable from hand.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TALON_GATES_TEXT: &str = "{4}: Put this card from your hand onto the battlefield.";

/// `n` units of colorless mana, as if already floating in a pool.
fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn talon_gates_ability_activatable_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Talon Gates of Madara in hand, with its from-hand activated ability.
    let gates_id = scenario
        .add_land_to_hand(P0, "Talon Gates of Madara")
        .from_oracle_text(TALON_GATES_TEXT)
        .id();
    // Four colorless mana floating to pay {4}.
    scenario.with_mana_pool(P0, floating_colorless(4));

    let mut runner = scenario.build();

    // The card is in hand before activation.
    assert_eq!(
        runner.state().objects[&gates_id].zone,
        Zone::Hand,
        "precondition: Talon Gates starts in hand",
    );

    // The AI candidate generator (`legal_actions`) must offer the ability — it
    // shares `can_activate_ability_now`, which previously rejected it.
    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == gates_id
        )),
        "Talon Gates' from-hand ability must be a legal ActivateAbility action; \
         legal_actions returned {actions:?}",
    );

    // Drive the real activation: handle_activate_ability → zone check →
    // pay {4} (from the funded pool) → ChangeZone resolution.
    let outcome = runner.activate(gates_id, 0).resolve();

    outcome.assert_zone(&[gates_id], Zone::Battlefield);
}

#[test]
fn ordinary_battlefield_ability_unaffected() {
    // Negative control: a permanent on the battlefield with an ordinary
    // activated ability whose effect does NOT move the source out of a
    // non-battlefield zone must remain activatable from the battlefield and
    // carry no derived activation zone.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let beast_id = scenario
        .add_creature_from_oracle(P0, "Mana Beast", 1, 1, "{T}: Add {G}.")
        .id();

    let mut runner = scenario.build();

    // The {T}: Add {G} ability has no activation_zone → defaults to Battlefield.
    let ability = &runner.state().objects[&beast_id].abilities[0];
    assert_eq!(
        ability.activation_zone, None,
        "an ordinary battlefield ability must not derive an activation zone",
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: beast_id,
            ability_index: 0,
        })
        .expect("an ordinary battlefield-activated ability must still activate");

    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        1,
        "the mana ability resolved: {{G}} in the pool",
    );
}

//! Integration test for issue #442 — Tyvar, Jubilant Brawler.
//!
//! Oracle (static):
//!   "You may activate abilities of creatures you control as though those
//!    creatures had haste."
//!
//! The static previously fell to `Effect::Unimplemented` (`statics: []`), so a
//! summoning-sick mana-creature (e.g., Dryad Arbor) could not tap for mana even
//! with Tyvar in play.
//!
//! The fix adds `StaticMode::CanActivateAbilitiesAsThoughHaste` and routes the
//! CR 602.5a summoning-sickness gate — for BOTH the activation-time check and
//! mana-source candidate generation — through the shared
//! `restrictions::summoning_sick_for_tap_ability` predicate, which consults the
//! static.
//!
//! These tests drive the real engine: candidate generation
//! (`activatable_mana_options` / `land_mana_options`) and the `apply` pipeline.
//! No hand-constructed expected state.

use engine::game::mana_sources::{activatable_land_mana_options, activatable_mana_options};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const TYVAR_STATIC_TEXT: &str =
    "You may activate abilities of creatures you control as though those creatures had haste.";

const MANA_DORK_TEXT: &str = "{T}: Add {G}.";

/// CR 602.5a: Without Tyvar's static, a summoning-sick `{T}: Add {G}` creature
/// produces no activatable mana option. With the static on a controlled
/// permanent, the option is generated and `apply` lets the creature tap for
/// `{G}`.
#[test]
fn summoning_sick_mana_creature_taps_with_tyvar_static() {
    // --- Without the static: rejected ---
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dork_id = scenario
        .add_creature_from_oracle(P0, "Dork", 0, 1, MANA_DORK_TEXT)
        .with_summoning_sickness()
        .id();
    let runner = scenario.build();
    assert!(
        activatable_mana_options(runner.state(), dork_id, P0).is_empty(),
        "a summoning-sick {{T}}: Add {{G}} creature must NOT tap for mana without Tyvar",
    );

    // --- With the static: permitted, and the engine taps it for {G} ---
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dork_id = scenario
        .add_creature_from_oracle(P0, "Dork", 0, 1, MANA_DORK_TEXT)
        .with_summoning_sickness()
        .id();
    // Tyvar's static, hosted on a controlled permanent.
    scenario
        .add_creature_from_oracle(P0, "Tyvar Source", 0, 1, TYVAR_STATIC_TEXT)
        .id();

    let mut runner = scenario.build();
    assert!(
        !activatable_mana_options(runner.state(), dork_id, P0).is_empty(),
        "Tyvar's static must lift the CR 602.5a gate — the summoning-sick \
         creature must now offer a mana option",
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: dork_id,
            ability_index: 0,
        })
        .expect("activating the mana ability must succeed with Tyvar's static in play");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "the summoning-sick creature must tap for {{G}} with Tyvar's static",
    );
    assert!(
        runner.state().objects[&dork_id].tapped,
        "the mana source must be tapped after activating its {{T}} ability",
    );
}

/// CR 602.5a: the Dryad Arbor path — a summoning-sick Land *creature* — is
/// gated in the land-mana candidate path (`activatable_land_mana_options`).
/// Tyvar's static lifts that gate too, since both candidate-generation paths
/// share the `summoning_sick_for_tap_ability` predicate.
#[test]
fn summoning_sick_land_creature_taps_with_tyvar_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arbor_id = scenario
        .add_creature_from_oracle(P0, "Dryad Arbor", 1, 1, MANA_DORK_TEXT)
        .with_summoning_sickness()
        .id();
    scenario
        .add_creature_from_oracle(P0, "Tyvar Source", 0, 1, TYVAR_STATIC_TEXT)
        .id();

    let mut runner = scenario.build();
    // Make the Arbor a Land in addition to being a Creature (Dryad Arbor's
    // printed type line).
    {
        let arbor = runner.state_mut().objects.get_mut(&arbor_id).unwrap();
        arbor.card_types.core_types.push(CoreType::Land);
        arbor.base_card_types = arbor.card_types.clone();
    }

    assert!(
        !activatable_land_mana_options(runner.state(), arbor_id, P0).is_empty(),
        "Tyvar's static must permit a summoning-sick Dryad-Arbor-shaped land \
         creature to be a land-mana option",
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: arbor_id,
            ability_index: 0,
        })
        .expect("Dryad Arbor must tap for {G} with Tyvar's static in play");
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "the summoning-sick land creature must tap for {{G}}",
    );
}

/// CR 602.5a applies to non-mana `{T}` abilities too — the gate Tyvar lifts is
/// the generic summoning-sickness restriction on `{T}`/`{Q}` activated
/// abilities, not just mana abilities.
#[test]
fn summoning_sick_tap_draw_ability_activates_with_tyvar_static() {
    // Without the static, activating the {T}: draw ability is rejected.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drawer_id = scenario
        .add_creature_from_oracle(P0, "Card Drawer", 0, 1, "{T}: Draw a card.")
        .with_summoning_sickness()
        .id();
    let mut runner = scenario.build();
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: drawer_id,
                ability_index: 0,
            })
            .is_err(),
        "a summoning-sick {{T}}: draw ability must be rejected without Tyvar",
    );

    // With the static, the ability resolves and draws a card.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drawer_id = scenario
        .add_creature_from_oracle(P0, "Card Drawer", 0, 1, "{T}: Draw a card.")
        .with_summoning_sickness()
        .id();
    scenario
        .add_creature_from_oracle(P0, "Tyvar Source", 0, 1, TYVAR_STATIC_TEXT)
        .id();
    scenario.add_card_to_library_top(P0, "Library Card");
    let mut runner = scenario.build();

    // The {T}: draw ability goes on the stack and resolves; the harness drives
    // it to completion and reports the net cards drawn since stack commit.
    let outcome = runner.activate(drawer_id, 0).resolve();
    outcome.assert_hand_drawn(P0, 1);
}

/// CR 109.4: Tyvar's static is controller-scoped ("creatures you control"). An
/// opponent's summoning-sick mana creature must NOT benefit.
#[test]
fn tyvar_static_does_not_help_opponent_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Tyvar's static is controlled by P0.
    scenario
        .add_creature_from_oracle(P0, "Tyvar Source", 0, 1, TYVAR_STATIC_TEXT)
        .id();
    // The opponent's summoning-sick mana creature.
    let opp_dork_id = scenario
        .add_creature_from_oracle(P1, "Opponent Dork", 0, 1, MANA_DORK_TEXT)
        .with_summoning_sickness()
        .id();

    let runner = scenario.build();
    assert!(
        activatable_mana_options(runner.state(), opp_dork_id, P1).is_empty(),
        "Tyvar's static only affects ITS controller's creatures — the \
         opponent's summoning-sick mana creature must still be gated",
    );
}

/// The static-mode query itself: with the static present, the engine reports
/// `CanActivateAbilitiesAsThoughHaste` true for a controlled creature and false
/// for an opponent's.
#[test]
fn check_static_ability_scopes_to_controller() {
    use engine::game::static_abilities::{check_static_ability, StaticCheckContext};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Tyvar Source", 0, 1, TYVAR_STATIC_TEXT)
        .id();
    let mine_id = scenario.add_creature(P0, "My Creature", 1, 1).id();
    let theirs_id = scenario.add_creature(P1, "Their Creature", 1, 1).id();

    let runner = scenario.build();

    assert!(
        check_static_ability(
            runner.state(),
            StaticMode::CanActivateAbilitiesAsThoughHaste,
            &StaticCheckContext {
                target_id: Some(mine_id),
                ..Default::default()
            },
        ),
        "the static must apply to a creature the static's controller controls",
    );
    assert!(
        !check_static_ability(
            runner.state(),
            StaticMode::CanActivateAbilitiesAsThoughHaste,
            &StaticCheckContext {
                target_id: Some(theirs_id),
                ..Default::default()
            },
        ),
        "the static must NOT apply to an opponent's creature",
    );
}

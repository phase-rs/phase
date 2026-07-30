//! Regression (issue #822): Erode / Path to Exile — "its controller may
//! search" prompts the destroyed/exiled creature's OWNER instead of its
//! CONTROLLER when the two differ (a stolen / control-changed creature).
//!
//! Erode: "Destroy target creature or planeswalker. Its controller may search
//! their library for a basic land card, put it onto the battlefield tapped,
//! then shuffle."
//!
//! Path to Exile: "Exile target creature. Its controller may search their
//! library for a basic land card, put that card onto the battlefield tapped,
//! then shuffle."
//!
//! CR 608.2c: the "its controller" anaphor binds to the controller of the
//! parent ability's chosen target. `optional_prompt_player`'s `SearchLibrary`
//! branch (`crates/engine/src/game/effects/mod.rs`) previously read the
//! target object's LIVE `controller` field directly instead of routing
//! through the LKI-aware `resolve_effect_player_ref` central resolver (the
//! same resolver the neighboring `Sacrifice` branch already used). Because
//! Destroy/Exile move the target off the battlefield before this optional
//! gate is evaluated, and `reset_for_battlefield_exit` resets the object's
//! `base_controller` back to its owner at that point, a live read silently
//! returns the OWNER whenever a stolen creature — controller different from
//! owner — is the one destroyed or exiled.
//!
//! Both tests install a real Layer 2 `ChangeController` continuous effect
//! (mirroring a Threaten/Act of Treason-style steal) rather than mutating
//! `GameObject.controller` directly, and drive the raw `GameAction` pipeline
//! (rather than the `SpellCast` builder's `.resolve()`, which auto-decides
//! `OptionalEffectChoice` per its `ResolutionPolicy` and would resolve straight
//! past the exact state this bug lives in) so the test can inspect
//! `WaitingFor::OptionalEffectChoice.player` directly — the bug's actual
//! prompt-routing seam (`optional_prompt_player`), not just the
//! already-correct search-resolution seam
//! (`search_library::resolve_library_owner`).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_white_mana(runner: &mut engine::game::scenario::GameRunner, player: usize) {
    runner.state_mut().players[player]
        .mana_pool
        .add(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
}

/// Steal `victim` from its owner: install a Threaten/Act of Treason-style
/// Layer 2 `ChangeController` effect routing control to `new_controller`, and
/// re-run the layer system so it actually takes effect (mirrors
/// `crates/engine/src/database/synthesis.rs`'s
/// `persist_returns_under_owner_not_controller_after_control_grab`).
fn steal_control(
    runner: &mut engine::game::scenario::GameRunner,
    victim: ObjectId,
    new_controller: engine::types::PlayerId,
) {
    runner.state_mut().add_transient_continuous_effect(
        victim,
        new_controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: victim },
        vec![ContinuousModification::ChangeController],
        None,
    );
    evaluate_layers(runner.state_mut());
}

/// Cast `spell` (an instant already in P0's hand, with white mana floating)
/// targeting `victim`, and drive priority until the stack empties or an
/// unhandled prompt (e.g. `OptionalEffectChoice`) halts the loop.
fn cast_targeting(
    runner: &mut engine::game::scenario::GameRunner,
    spell: ObjectId,
    victim: ObjectId,
) {
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must succeed");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .expect("targeting the victim creature must succeed");
    }

    runner.advance_until_stack_empty();
}

#[test]
fn erode_search_prompt_routes_to_stolen_creatures_controller_not_owner() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let erode = scenario.add_real_card(P0, "Erode", Zone::Hand, db);
    // P0 owns this creature, but P1 will steal control of it before it dies.
    let victim = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let p1_forest = scenario.add_real_card(P1, "Forest", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_white_mana(&mut runner, 0);

    steal_control(&mut runner, victim, P1);
    assert_eq!(
        runner.state().objects[&victim].owner,
        P0,
        "precondition: P0 remains the owner"
    );
    assert_eq!(
        runner.state().objects[&victim].controller,
        P1,
        "precondition: P1 controls the stolen creature"
    );

    cast_targeting(&mut runner, erode, victim);

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "Erode must destroy the targeted creature"
    );

    // The core fix: the "may search" prompt must route to P1 — the
    // creature's controller at the moment it died — not P0, its owner.
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(
            *player, P1,
            "Erode's 'its controller may search' must prompt the CONTROLLER \
             at time of death (P1), not the owner (P0)",
        ),
        other => panic!("expected OptionalEffectChoice after Erode resolves, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepting the optional search must succeed");

    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(
                *player, P1,
                "SearchChoice must also route to the stolen creature's controller (P1)"
            );
            assert!(
                cards.contains(&p1_forest),
                "P1's own Forest must be a legal basic-land search choice"
            );
        }
        other => panic!("expected SearchChoice after Erode resolves, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_forest],
        })
        .expect("P1 selecting the Forest must resolve the search continuation");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&p1_forest].zone,
        Zone::Battlefield,
        "the found basic land must enter the battlefield"
    );
    assert_eq!(
        runner.state().objects[&p1_forest].controller,
        P1,
        "the found land enters under the searching player's (P1's) control"
    );
}

#[test]
fn path_to_exile_search_prompt_routes_to_stolen_creatures_controller_not_owner() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let path = scenario.add_real_card(P0, "Path to Exile", Zone::Hand, db);
    let victim = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let p1_forest = scenario.add_real_card(P1, "Forest", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_white_mana(&mut runner, 0);

    steal_control(&mut runner, victim, P1);
    assert_eq!(runner.state().objects[&victim].owner, P0);
    assert_eq!(runner.state().objects[&victim].controller, P1);

    cast_targeting(&mut runner, path, victim);

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "Path to Exile must exile the targeted creature"
    );

    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(
            *player, P1,
            "Path to Exile's 'its controller may search' must prompt the \
             CONTROLLER at time of exile (P1), not the owner (P0)",
        ),
        other => {
            panic!("expected OptionalEffectChoice after Path to Exile resolves, got {other:?}")
        }
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepting the optional search must succeed");

    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(
                *player, P1,
                "SearchChoice must route to P1, not the owner P0"
            );
            assert!(cards.contains(&p1_forest));
        }
        other => panic!("expected SearchChoice after Path to Exile resolves, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_forest],
        })
        .expect("P1 selecting the Forest must resolve the search continuation");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&p1_forest].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&p1_forest].controller, P1);
}

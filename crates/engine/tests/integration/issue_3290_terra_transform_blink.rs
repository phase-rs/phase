//! Regression for GitHub issue #3290 — Terra should revert to its front face
//! after leaving the battlefield and blinking back.
//!
//! CR 712.14: A transforming double-faced card can only be transformed on the
//! battlefield. When it leaves the battlefield, it reverts to its front face.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_flickerwisp_mana(runner: &mut engine::game::scenario::GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in [ManaType::White, ManaType::White, ManaType::Colorless] {
        pool.add(ManaUnit::new(m, dummy, false, vec![]));
    }
}

#[test]
fn terra_transformed_blink_returns_front_face() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let terra = scenario.add_real_card(P0, "Terra, Magical Adept", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let obj = runner.state().objects.get(&terra).expect("Terra exists");
        assert!(
            obj.back_face.is_some(),
            "precondition: Terra must have Esper Terra as its DFC back face"
        );
        assert_eq!(
            obj.back_face.as_ref().unwrap().name,
            "Esper Terra",
            "Terra, Magical Adept transforms into Esper Terra per FIN #245"
        );
    }

    let mut events = Vec::new();
    engine::game::transform::transform_permanent(runner.state_mut(), terra, &mut events).unwrap();
    {
        let obj = runner.state().objects.get(&terra).expect("Terra exists");
        assert!(obj.transformed, "precondition: Terra starts transformed");
        assert_eq!(obj.name, "Esper Terra");
    }

    // Blink: exile, then return without `enter_transformed`.
    engine::game::zones::move_to_zone(runner.state_mut(), terra, Zone::Exile, &mut events);
    {
        let obj = runner.state().objects.get(&terra).expect("Terra in exile");
        assert!(
            !obj.transformed,
            "Terra must revert to front face in exile (CR 712.14)"
        );
        assert_eq!(obj.name, "Terra, Magical Adept");
    }

    engine::game::zones::move_to_zone(runner.state_mut(), terra, Zone::Battlefield, &mut events);
    {
        let obj = runner.state().objects.get(&terra).expect("Terra returned");
        assert!(
            !obj.transformed,
            "Terra must enter the battlefield on its front face after a blink"
        );
        assert_eq!(obj.name, "Terra, Magical Adept");
    }
}

/// Flickerwisp blink is the production path from the Discord report: exile now,
/// return at the next end step without `enter_transformed`.
#[test]
fn terra_transformed_flickerwisp_returns_front_face_at_end_step() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flickerwisp = scenario.add_real_card(P0, "Flickerwisp", Zone::Hand, db);
    let terra = scenario.add_real_card(P1, "Terra, Magical Adept", Zone::Battlefield, db);
    for _ in 0..20 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let mut events = Vec::new();
    engine::game::transform::transform_permanent(runner.state_mut(), terra, &mut events).unwrap();
    assert!(runner.state().objects[&terra].transformed);
    assert_eq!(runner.state().objects[&terra].name, "Esper Terra");

    add_flickerwisp_mana(&mut runner);
    let card_id = runner.state().objects[&flickerwisp].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flickerwisp,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Flickerwisp cast");

    let mut guard = 0;
    while runner.state().delayed_triggers.is_empty() {
        guard += 1;
        assert!(
            guard < 64,
            "ETB trigger stalled: {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(terra)),
                    })
                    .expect("choose Terra");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    assert_eq!(runner.state().objects[&terra].zone, Zone::Exile);
    assert!(
        !runner.state().objects[&terra].transformed,
        "Terra must revert to front face while in exile"
    );

    guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 256, "delayed return stalled");
        runner
            .act(GameAction::PassPriority)
            .expect("pass to end step");
    }

    let terra_obj = &runner.state().objects[&terra];
    assert_eq!(terra_obj.zone, Zone::Battlefield);
    assert!(
        !terra_obj.transformed,
        "Terra must return on its front face after a Flickerwisp blink"
    );
    assert_eq!(terra_obj.name, "Terra, Magical Adept");
}

/// Debug `SetFaceState` must swap DFC faces, not just toggle the flag.
#[test]
fn terra_debug_transform_then_blink_returns_front_face() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let terra = scenario.add_real_card(P0, "Terra, Magical Adept", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    runner
        .act(GameAction::Debug(DebugAction::SetFaceState {
            object_id: terra,
            face_down: None,
            transformed: Some(true),
            flipped: None,
        }))
        .expect("debug transform");
    assert!(runner.state().objects[&terra].transformed);
    assert_eq!(runner.state().objects[&terra].name, "Esper Terra");

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), terra, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), terra, Zone::Battlefield, &mut events);

    let obj = runner.state().objects.get(&terra).unwrap();
    assert!(!obj.transformed);
    assert_eq!(obj.name, "Terra, Magical Adept");
}

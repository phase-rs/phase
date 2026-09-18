//! CR 601.3 + CR 611.3a: the direct-object top-of-library play permission —
//! "you may play the top card of your library" (The Lunar Whale's effect
//! clause) — must parse into a conditional `TopOfLibraryCastPermission` and
//! gate library-top plays on its typed condition. The condition used here
//! ("~ is untapped") exercises the same gate seam The Lunar Whale's
//! "~ attacked this turn" clause will use once that condition arm lands.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{StaticCondition, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const OBJECT_FORM_BEACON_ORACLE: &str =
    "As long as ~ is untapped, you may play the top card of your library.";

fn play_land_offered(state: &GameState, top_id: ObjectId) -> bool {
    engine::ai_support::legal_actions(state)
        .iter()
        .any(|a| matches!(a, GameAction::PlayLand { object_id, .. } if *object_id == top_id))
}

/// SHAPE: the parsed static is the permission class with `affected: Any` (no
/// eligibility filter — the verb's object names the top card itself) and the
/// typed gate attached rather than dropped.
#[test]
fn object_form_permission_carries_typed_gate() {
    let mut scenario = GameScenario::new();
    let beacon = scenario
        .add_creature(P0, "Object Form Beacon", 2, 2)
        .from_oracle_text(OBJECT_FORM_BEACON_ORACLE)
        .id();
    let runner = scenario.build();

    let permission = runner
        .state()
        .objects
        .get(&beacon)
        .unwrap()
        .static_definitions
        .iter_unchecked()
        .find(|d| matches!(d.mode, StaticMode::TopOfLibraryCastPermission { .. }))
        .expect("object form must produce a TopOfLibraryCastPermission static");
    assert_eq!(
        permission.affected,
        Some(TargetFilter::Any),
        "the object form has no eligibility filter"
    );
    assert_eq!(
        permission.condition,
        Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::SourceIsTapped),
        }),
        "the printed gate must be attached, not dropped"
    );
}

/// CR 611.3a: the live gate decides availability — the library-top land is
/// offered while the source is untapped and is no longer offered once the
/// source is tapped. Reverting the parser arm removes the permission entirely,
/// so the positive half fails.
#[test]
fn object_form_permission_gates_land_play_on_source_state() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let beacon = scenario
        .add_creature(P0, "Object Form Beacon", 2, 2)
        .from_oracle_text(OBJECT_FORM_BEACON_ORACLE)
        .id();
    let top_id = scenario.add_card_to_library_top(P0, "Object Form Test Land");
    let mut runner = scenario.build();

    // CR 601.1a + CR 305.1: "playing" covers land plays, so a land on top of
    // the library is the `Play`-mode probe.
    {
        let obj = runner.state_mut().objects.get_mut(&top_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    assert!(
        play_land_offered(runner.state(), top_id),
        "the permission must surface the top land while the source is untapped"
    );

    runner.state_mut().objects.get_mut(&beacon).unwrap().tapped = true;
    assert!(
        !play_land_offered(runner.state(), top_id),
        "tapping the source must revoke the permission (CR 611.3a re-evaluation)"
    );
}

/// CR 305.1 + CR 601.3: the permission is executable end to end — the offered
/// `PlayLand` action moves the top land to the battlefield, proving the
/// library-top play runs the real land-play pipeline rather than only
/// appearing in the legal-action list.
#[test]
fn object_form_permission_plays_the_top_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _beacon = scenario
        .add_creature(P0, "Object Form Beacon", 2, 2)
        .from_oracle_text(OBJECT_FORM_BEACON_ORACLE)
        .id();
    let top_id = scenario.add_card_to_library_top(P0, "Object Form Test Land");
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&top_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }

    let card_id = runner.state().objects.get(&top_id).unwrap().card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: top_id,
            card_id,
        })
        .expect("the object-form permission must allow playing the top land");
    assert_eq!(
        runner.state().objects.get(&top_id).unwrap().zone,
        Zone::Battlefield,
        "the played land must leave the library for the battlefield"
    );
}

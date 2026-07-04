//! Regression: PR #3949 maintainer review — top-of-library casts must consume a
//! once-per-turn permission slot ONLY when that bounded permission is what
//! authorized the cast.
//!
//! Bug: with BOTH an `Unlimited` top-of-library permission (Future Sight /
//! Realmwalker / Bolas's Citadel shape) AND a `OncePerTurn` one (Assemble the
//! Players / Johann shape) whose filters both match the top card, casting the
//! top spell burned the once-per-turn slot — even though the unlimited
//! permission alone authorized the cast — and incorrectly hid the next matching
//! top spell for the rest of the turn.
//!
//! Fix: the casting pipeline now threads the *selected* authorizing permission
//! source/frequency (preferring an `Unlimited` authorizer) and consumes the
//! bounded slot only when the selected frequency is `OncePerTurn`.
//!
//! CR 601.2a + CR 401.5: a bounded per-turn cast slot is spent only when that
//! specific bounded permission authorizes the cast; an unlimited permission
//! that also matches suffices on its own and preserves the bounded slot.
//!
//! This drives the REAL production cast path (`GameAction::CastSpell` through
//! `GameRunner::act` → `finalize_cast`). The two permissions are installed as
//! synthetic `TopOfLibraryCastPermission` statics on battlefield permanents so
//! the test does not depend on Assemble/Johann being present in the curated
//! fixture; the cast target is a real {0} creature (Memnite) whose
//! characteristics match both filters and which casts for free.

use engine::types::ability::{
    CardPlayMode, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::support::shared_card_db as load_db;

/// Move an object to the front of its owner's library so it is the "top card."
fn move_to_top_of_library(
    state: &mut engine::types::game_state::GameState,
    obj_id: ObjectId,
    owner: PlayerId,
) {
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.push_front(obj_id);
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.zone = Zone::Library;
}

/// Install a `TopOfLibraryCastPermission` static (with the given frequency)
/// whose `affected` filter admits any creature onto a fresh battlefield
/// permanent. Returns that permanent's `ObjectId` (the per-turn slot key).
fn install_top_of_library_creature_static(
    state: &mut engine::types::game_state::GameState,
    controller: PlayerId,
    frequency: CastFrequency,
    card_id: CardId,
) -> ObjectId {
    let src = engine::game::zones::create_object(
        state,
        card_id,
        controller,
        format!("TopLib permission ({frequency})"),
        Zone::Battlefield,
    );
    let def = StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
        play_mode: CardPlayMode::Cast,
        frequency,
        alt_cost: None,
    })
    .affected(TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: None,
        properties: vec![],
    }));
    state
        .objects
        .get_mut(&src)
        .unwrap()
        .static_definitions
        .push(def);
    src
}

/// CR 601.2a + CR 401.5: casting the top spell while BOTH an `Unlimited` and a
/// `OncePerTurn` top-of-library permission match it must NOT consume the
/// once-per-turn slot — the unlimited permission alone authorizes the cast — so
/// a second matching top spell stays castable this turn.
///
/// DISCRIMINATING ASSERTION: `top_of_library_cast_permissions_used` must NOT
/// contain the once-per-turn source after the cast. Reverting the threading fix
/// (back to the independent first-`OncePerTurn`-match rescan at finalize) makes
/// the cast stamp that source, so the set WOULD contain it and the second
/// castability assertion would also fail.
#[test]
fn mixed_unlimited_and_once_per_turn_does_not_burn_bounded_slot() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two distinct top-of-library creatures (Memnite is a {0} artifact creature
    // — matches the creature filter and casts for free).
    let first_top = scenario.add_real_card(P0, "Memnite", Zone::Library, db);
    let second_top = scenario.add_real_card(P0, "Memnite", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Install BOTH permissions. Order the OncePerTurn one FIRST in battlefield
    // iteration so a naive first-match would pick it; the Unlimited-preference
    // must still win.
    let once_src = install_top_of_library_creature_static(
        runner.state_mut(),
        P0,
        CastFrequency::OncePerTurn,
        CardId(7_701),
    );
    let _unlimited_src = install_top_of_library_creature_static(
        runner.state_mut(),
        P0,
        CastFrequency::Unlimited,
        CardId(7_702),
    );

    move_to_top_of_library(runner.state_mut(), first_top, P0);

    // Sanity: the top card is castable before any cast.
    let available = engine::game::casting::spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        available.contains(&first_top),
        "the top creature must be castable with both permissions active"
    );

    // Cast the top spell through the REAL cast pipeline.
    let card_id = runner.state().objects[&first_top].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: first_top,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("casting the {0} top creature via the unlimited permission should succeed");

    // CR 601.2a: the bounded OncePerTurn slot must NOT be consumed — the
    // Unlimited permission authorized the cast.
    assert!(
        !runner
            .state()
            .top_of_library_cast_permissions_used
            .contains(&once_src),
        "Unlimited permission authorized the cast — the OncePerTurn slot must be preserved, \
         but it was stamped: used_set={:?}",
        runner.state().top_of_library_cast_permissions_used,
    );

    // The first Memnite has left the library for the stack.
    assert_ne!(
        runner.state().objects[&first_top].zone,
        Zone::Library,
        "the cast top card must have moved off the top of library"
    );

    // Put the second Memnite on top and verify it is STILL castable — the
    // once-per-turn slot was preserved, so the offer remains.
    move_to_top_of_library(runner.state_mut(), second_top, P0);
    let available_after =
        engine::game::casting::spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        available_after.contains(&second_top),
        "a second matching top spell must remain castable when no bounded slot was spent"
    );
}

/// CR 601.2a + CR 401.5: control case — with ONLY a `OncePerTurn` permission,
/// casting the top spell DOES consume the slot, and the next matching top spell
/// is hidden until the slot resets. This is the existing once-per-turn behavior
/// the fix must keep passing, exercised through the same production cast path.
///
/// DISCRIMINATING ASSERTION: after the cast, `top_of_library_cast_permissions_used`
/// contains the source AND the second top spell is NOT castable. If the fix
/// wrongly skipped consumption for the bounded-only case, the used-set would be
/// empty and the second offer would remain — flipping both assertions.
#[test]
fn pure_once_per_turn_burns_slot_and_hides_next_top_spell() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_top = scenario.add_real_card(P0, "Memnite", Zone::Library, db);
    let second_top = scenario.add_real_card(P0, "Memnite", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let once_src = install_top_of_library_creature_static(
        runner.state_mut(),
        P0,
        CastFrequency::OncePerTurn,
        CardId(7_701),
    );

    move_to_top_of_library(runner.state_mut(), first_top, P0);

    let card_id = runner.state().objects[&first_top].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: first_top,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("casting the {0} top creature via the once-per-turn permission should succeed");

    // CR 601.2a: the bounded slot IS consumed — it was the only authorizer.
    assert!(
        runner
            .state()
            .top_of_library_cast_permissions_used
            .contains(&once_src),
        "OncePerTurn permission authorized the cast — its slot must be consumed"
    );

    // The next matching top spell is hidden for the rest of the turn.
    move_to_top_of_library(runner.state_mut(), second_top, P0);
    let available_after =
        engine::game::casting::spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !available_after.contains(&second_top),
        "with the once-per-turn slot spent, the next top spell must be hidden this turn"
    );
}

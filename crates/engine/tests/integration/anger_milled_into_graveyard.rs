//! Issue #10400 — a static whose zone of function is a non-battlefield zone is
//! not applied when its own card moves into that zone during a resolution.
//!
//! Oracle (Anger, verbatim from the shipped fixture): "Haste\nAs long as this
//! card is in your graveyard and you control a Mountain, creatures you control
//! have haste." CR 113.6b: an ability that states which zones it functions in
//! functions only from those zones; CR 611.3a: the continuous effect isn't
//! "locked in", so a change of where the static functions must re-derive every
//! recipient (CR 613.1).
//!
//! Every row drives the production zone-change pipeline
//! (`zone_pipeline::move_object` over a `ZoneMoveRequest`, reached cross-crate
//! through `move_object_for_test`): that is the entry point `Effect::Mill`
//! builds per card, per `masters_councillors::mill_cards_into_graveyard`. The
//! Library origin is the discriminating one — `zones.rs` marks `Full`
//! unconditionally for Hand and Battlefield transitions, so a Hand-origin mill
//! could not distinguish the per-object zone-of-function term at all.
//!
//! Grist, the Hunger Tide is the class witness with a different layer and a
//! different gate shape (CR 113.6c negation: `active_zones` is every
//! non-battlefield zone, so it also crosses on a Library → Graveyard mill).
//! Bow of Nylea is the sibling-primitive witness: its fourth mode puts a
//! graveyard card on the bottom of a library through
//! `zones::move_to_library_at_index`, which never passes through
//! `move_to_zone`.

use crate::support::shared_card_db;
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Mill `id` from its library into its owner's graveyard through the production
/// pipeline (the exact request `effects/mill.rs` builds), asserting the
/// delivery terminated and landed so downstream negative assertions are not
/// vacuous.
fn mill_from_library(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    let paused = move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(id, Zone::Graveyard, id),
        &mut events,
    );
    assert!(
        !paused,
        "the mill delivery must terminate, not park on a replacement choice"
    );
    assert_eq!(
        runner.state().objects[&id].zone,
        Zone::Graveyard,
        "CR 701.17a: the pipeline must deliver the milled card to the graveyard"
    );
}

/// Real Anger in `Zone::Library`, a real Mountain on the battlefield when
/// `with_mountain`, and a synthetic 2/2 for P0. Returns `(runner, anger, bear)`.
fn anger_board(with_mountain: bool) -> (GameRunner, ObjectId, ObjectId) {
    let db = shared_card_db().expect("this row requires the integration CardDB");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    if with_mountain {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let anger = scenario.add_real_card(P0, "Anger", Zone::Library, db);
    let runner = scenario.build();
    (runner, anger, bear)
}

fn has_haste(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Haste)
}

/// V7 — THE ISSUE'S REPRO. A real Anger milled Library → Graveyard must mark
/// layers at the seam (before any action-boundary flush) and then grant Haste
/// to a creature that already existed.
#[test]
fn milled_anger_marks_layers_and_grants_haste() {
    let (mut runner, anger, bear) = anger_board(true);

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(
        !runner.state().layers_dirty.is_dirty(),
        "precondition: the cache is clean before the mill"
    );
    assert!(
        !has_haste(&runner, bear),
        "precondition: no Anger in the graveyard yet, so no Haste"
    );

    mill_from_library(&mut runner, anger);

    assert!(
        runner.state().layers_dirty.is_dirty(),
        "THE FIX: milling the source into the zone its declaration names must \
         invalidate layers at the seam"
    );
    flush_layers(runner.state_mut());
    assert!(
        has_haste(&runner, bear),
        "a pre-existing creature gains Haste once Anger is in the graveyard \
         with a Mountain controlled"
    );
}

/// V8 — negative half, paired with V7. Without a Mountain the mill must still
/// mark (the zone-of-function crossing happened) and must still grant nothing.
#[test]
fn milled_anger_without_mountain_marks_but_grants_nothing() {
    let (mut runner, anger, bear) = anger_board(false);

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(!runner.state().layers_dirty.is_dirty());

    mill_from_library(&mut runner, anger);

    assert!(
        runner.state().layers_dirty.is_dirty(),
        "the crossing marks even when the gate stays false"
    );
    flush_layers(runner.state_mut());
    assert!(
        !has_haste(&runner, bear),
        "without a Mountain the compound condition fails and no Haste applies"
    );
}

/// V9 — class witness with a different layer AND a different gate shape: Grist,
/// the Hunger Tide is a 1/1 Insect creature card off the battlefield (CR 113.6c
/// negation). A milled Grist must mark, and after the flush the graveyard object
/// must carry the Type and P/T layers.
///
/// Two controls: (a) the same card placed on the battlefield by the same helper
/// is not a creature (the condition, not the card, is what decides); (b) a
/// forced `mark_layers_full` + flush on a freshly milled copy produces the
/// identical creature state — so this row fails only for the missing mark, not
/// for the layer machinery.
#[test]
fn milled_grist_marks_layers_and_becomes_a_creature_card() {
    let db = shared_card_db().expect("this row requires the integration CardDB");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grist = scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Library, db);
    let mut runner = scenario.build();

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    assert!(
        !runner.state().layers_dirty.is_dirty(),
        "precondition: the cache is clean before the mill"
    );

    mill_from_library(&mut runner, grist);
    assert!(
        runner.state().layers_dirty.is_dirty(),
        "the class witness must mark layers on the same seam"
    );
    flush_layers(runner.state_mut());
    let obj = &runner.state().objects[&grist];
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "a milled Grist is a creature card in the graveyard (got {:?})",
        obj.card_types.core_types
    );
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(1), Some(1)),
        "CR 113.6c + CR 613.1: the off-battlefield P/T layer applies"
    );

    // Control (a): on the battlefield the same static's condition is false.
    let mut battlefield_scenario = GameScenario::new();
    battlefield_scenario.at_phase(Phase::PreCombatMain);
    let on_battlefield =
        battlefield_scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Battlefield, db);
    let mut battlefield_runner = battlefield_scenario.build();
    flush_layers(battlefield_runner.state_mut());
    assert!(
        !battlefield_runner.state().objects[&on_battlefield]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "on the battlefield Grist is a planeswalker only"
    );

    // Control (b): the layer machinery derives the same state from a forced
    // mark, so the only thing V9's main half adds is the seam's mark.
    let mut forced_scenario = GameScenario::new();
    forced_scenario.at_phase(Phase::PreCombatMain);
    let forced_grist =
        forced_scenario.add_real_card(P0, "Grist, the Hunger Tide", Zone::Library, db);
    let mut forced_runner = forced_scenario.build();
    mill_from_library(&mut forced_runner, forced_grist);
    mark_layers_full(forced_runner.state_mut());
    flush_layers(forced_runner.state_mut());
    let forced = &forced_runner.state().objects[&forced_grist];
    assert!(
        forced.card_types.core_types.contains(&CoreType::Creature),
        "a forced full pass must derive the identical creature state"
    );
    assert_eq!((forced.power, forced.toughness), (Some(1), Some(1)));
}

/// V10 — exit direction with real card data. Anger milled (Haste present), then
/// exiled through the same pipeline: the grant must be withdrawn and the mark
/// must fire.
#[test]
fn exiled_anger_drops_its_graveyard_grant() {
    let (mut runner, anger, bear) = anger_board(true);
    mill_from_library(&mut runner, anger);
    flush_layers(runner.state_mut());
    assert!(
        has_haste(&runner, bear),
        "precondition: milled Anger grants Haste"
    );
    assert!(!runner.state().layers_dirty.is_dirty());

    let mut events = Vec::new();
    let paused = move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(anger, Zone::Exile, bear),
        &mut events,
    );
    assert!(!paused);
    assert_eq!(runner.state().objects[&anger].zone, Zone::Exile);

    assert!(
        runner.state().layers_dirty.is_dirty(),
        "exiling the source out of its declared zone must invalidate layers"
    );
    flush_layers(runner.state_mut());
    assert!(
        !has_haste(&runner, bear),
        "the Bear must lose the Haste its source no longer grants"
    );
}

/// V12 — SIBLING-PRIMITIVE row. Bow of Nylea's fourth mode ("Put up to four
/// target cards from your graveyard on the bottom of your library in any
/// order.") resolves into `zones::move_to_library_at_index`, which bypasses
/// `move_to_zone`'s dirty-mark block entirely.
///
/// Roster: real Bow of Nylea on the battlefield (activation `{1}{G}, {T}`,
/// paid from an explicit two-green pool), real Anger in the graveyard, real
/// Mountain on the battlefield, a synthetic 2/2 that has Haste before the
/// activation.
///
/// Revert: haste retained and no mark, because only the top-of-library
/// self-gate runs on this path.
#[test]
fn bow_of_nylea_bottom_placement_drops_anger_grant() {
    let db = shared_card_db().expect("this row requires the integration CardDB");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ],
    );
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let anger = scenario.add_real_card(P0, "Anger", Zone::Graveyard, db);
    let bow = scenario.add_real_card(P0, "Bow of Nylea", Zone::Battlefield, db);
    let mut runner = scenario.build();

    flush_layers(runner.state_mut());
    assert!(
        has_haste(&runner, bear),
        "reach guard: Anger in the graveyard already grants Haste before the activation"
    );
    assert_eq!(runner.state().objects[&anger].zone, Zone::Graveyard);

    let outcome = runner
        .activate(bow, 0)
        .modes(&[3])
        .target_objects(&[anger])
        .resolve();

    // The closing "in any order" may raise the arrangement prompt; submit the
    // single chosen card so the resolution completes.
    if matches!(
        outcome.final_waiting_for(),
        WaitingFor::EffectZoneChoice { .. }
    ) {
        runner
            .act(GameAction::SelectCards { cards: vec![anger] })
            .expect("P0 submits the arrangement");
    }

    // Reach guards: the placement ran, so the missing-Haste assertion is not
    // vacuous.
    assert_eq!(runner.state().objects[&anger].zone, Zone::Library);
    assert_eq!(runner.state().players[0].library.back(), Some(&anger));

    assert!(
        !has_haste(&runner, bear),
        "out of the graveyard, Anger no longer grants Haste to recipients"
    );
}

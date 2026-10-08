//! Runtime discriminating tests for **Gourmand's Talent** (BLC), the card that
//! motivates the leading-turn-window composition in
//! `oracle_static/dispatch.rs::parse_static_line_inner` and
//! `oracle_classifier.rs::is_static_pattern`.
//!
//! Gourmand's Talent (`Enchantment — Class`):
//!
//! ```text
//! (Gain the next level as a sorcery to add its ability.)
//! During your turn, artifacts you control are Foods in addition to their
//! other types and have "{2}, {T}, Sacrifice this artifact: You gain 3 life."
//! {2}{G}: Level 2
//! Whenever you gain life for the first time each turn, create a 3/3 green
//! Raccoon creature token.
//! {3}{G}: Level 3
//! Whenever you gain life for the first time each turn, put a +1/+1 counter
//! on each creature you control.
//! ```
//!
//! Before the fix, the level-1 line fell through every static parser (the
//! dispatcher's terminal position had no leading-window arm, and the Class
//! route's `is_static_pattern` classifier refused the line outright), so it
//! became an honest `Effect::Unimplemented` and the card did nothing.
//!
//! **B1 — the fixture must set the `Class` subtype BEFORE the Oracle text is
//! parsed.** `CardBuilder::from_oracle_text` is the call that runs
//! `parse_class_oracle_text` (via the Class pre-parser gate in
//! `parse_oracle_ir`), and it snapshots `card_types.subtypes` at call time. A
//! `.with_subtypes(vec!["Class"])` chained AFTER `.from_oracle_text(..)` would
//! be too late — the level bars would never be sectioned, and this file's
//! Test 3 paired positive (the level-2 trigger's `ClassLevelGE` condition)
//! would be unreachable. So every fixture below chains
//! `.with_subtypes(vec!["Class"])` BEFORE `.from_oracle_text(GOURMANDS_TALENT)`,
//! matching the repo idiom in
//! `issue_5330_scavengers_talent_graveyard_return.rs` and
//! `issue_6643_party_dude_opponents_attacked.rs`.
//!
//! CR 604.1 + CR 102.1 + CR 109.5 + CR 611.3a: a printed leading turn window
//! ("During your turn, ") gates WHEN a static ability's statement is true,
//! bound to the SOURCE's controller and re-evaluated live every layer pass.
//! CR 205.1b: "in addition to their other types" retains prior types (the
//! artifact stays an Artifact while also a Food). CR 700.7: the granted
//! quoted ability's "this artifact" is a `this [something]` self-reference to
//! the HOST it lands on, never the granting Class (distinct from CR 201.5a/b,
//! which are by-name rules and do not apply to this card).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, Effect, StaticCondition, TriggerCondition};
use engine::types::card_type::CoreType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const GOURMANDS_TALENT: &str = "(Gain the next level as a sorcery to add its ability.)\n\
During your turn, artifacts you control are Foods in addition to their other types and have \
\"{2}, {T}, Sacrifice this artifact: You gain 3 life.\"\n\
{2}{G}: Level 2\n\
Whenever you gain life for the first time each turn, create a 3/3 green Raccoon creature token.\n\
{3}{G}: Level 3\n\
Whenever you gain life for the first time each turn, put a +1/+1 counter on each creature you control.";

/// Shared fixture: P0's Gourmand's Talent (Class), a vanilla artifact P0
/// controls, and a vanilla artifact P1 controls (the owner-vs-controller
/// hostile row) — all on the battlefield, with mana to cover the granted
/// ability's `{2}` cost.
fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );

    let p0_artifact = scenario
        .add_artifact_from_oracle(P0, "Vanilla Artifact", "")
        .id();
    let p1_artifact = scenario
        .add_artifact_from_oracle(P1, "Opponent Artifact", "")
        .id();
    // B1: subtype BEFORE oracle text — `from_oracle_text` is the call that
    // runs `parse_class_oracle_text`, and it must see `Class` already set.
    let gourmand = scenario
        .add_creature(P0, "Gourmand's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(GOURMANDS_TALENT)
        .id();

    let runner = scenario.build();
    (runner, gourmand, p0_artifact, p1_artifact)
}

/// V10 + V12 + the owner/controller hostile row: the additive Food subtype
/// (and its granted sacrifice-for-life ability) appear on a controlled
/// artifact ONLY during its controller's turn, the Artifact type is retained
/// (CR 205.1b), and an opponent's simultaneously-battlefielded artifact never
/// receives the grant (CR 109.5) regardless of whose turn it is.
#[test]
fn food_subtype_only_during_controllers_turn() {
    let (mut runner, gourmand, p0_artifact, p1_artifact) = setup();

    // ----- Reach-guard: the level-1 line parsed to a real static, not a
    // dropped `Effect::Unimplemented`. Fails on revert (today: zero statics,
    // one Unimplemented). -----
    let food_static = runner.state().objects[&gourmand]
        .base_static_definitions
        .iter()
        .find(|s| {
            matches!(s.mode, StaticMode::Continuous)
                && s.modifications.iter().any(|m| {
                    matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Food")
                })
        })
        .cloned()
        .expect(
            "Gourmand's Talent level-1 line must parse to a Continuous static carrying \
             AddSubtype{Food}, not fall through to Effect::Unimplemented",
        );
    assert_eq!(
        food_static.condition,
        Some(StaticCondition::DuringYourTurn),
        "the level-1 static must be gated on the controller's turn: {:?}",
        food_static.condition
    );
    assert!(
        !runner.state().objects[&gourmand]
            .base_abilities
            .iter()
            .any(|a| matches!(a.effect.as_ref(), Effect::Unimplemented { .. })),
        "the level-1 line must no longer leave a residual Effect::Unimplemented: {:?}",
        runner.state().objects[&gourmand].base_abilities
    );

    // ----- P0's turn: the grant is live on P0's artifact only. -----
    runner.state_mut().active_player = P0;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let p0_obj = &runner.state().objects[&p0_artifact];
    assert!(
        p0_obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "P0's artifact must be a Food on P0's turn: {:?}",
        p0_obj.card_types.subtypes
    );
    assert!(
        p0_obj.card_types.core_types.contains(&CoreType::Artifact),
        "CR 205.1b: the Artifact type must be retained alongside Food: {:?}",
        p0_obj.card_types.core_types
    );
    assert_eq!(
        p0_obj.abilities.len(),
        1,
        "P0's artifact must carry exactly the one granted sacrifice-for-life ability: {:?}",
        p0_obj.abilities
    );

    // Owner-vs-controller hostile row (CR 109.5): P1's artifact, on the
    // battlefield at the same time, must receive neither the subtype nor the
    // ability while it is P0's turn.
    let p1_obj = &runner.state().objects[&p1_artifact];
    assert!(
        !p1_obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "an opponent's artifact must not be granted Food: {:?}",
        p1_obj.card_types.subtypes
    );
    assert!(
        p1_obj.abilities.is_empty(),
        "an opponent's artifact must not be granted the ability: {:?}",
        p1_obj.abilities
    );

    // ----- V12, THE TURN DISCRIMINATOR: on the opponent's turn, the whole
    // effect is off. Fails on revert AND under any fix that drops the window. -----
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let p0_obj = &runner.state().objects[&p0_artifact];
    assert!(
        !p0_obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "on the opponent's turn P0's artifact must NOT be a Food: {:?}",
        p0_obj.card_types.subtypes
    );
    assert!(
        p0_obj.abilities.is_empty(),
        "on the opponent's turn P0's artifact must have no granted ability: {:?}",
        p0_obj.abilities
    );
}

/// V11: the granted activated ability activates for exactly +3 life and
/// sacrifices the HOST artifact, not the granting Class (CR 700.7's `this
/// [something]` self-reference binds to the object it's on).
#[test]
fn granted_food_ability_activates_and_sacrifices_the_host() {
    let (mut runner, gourmand, p0_artifact, _p1_artifact) = setup();

    runner.state_mut().active_player = P0;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let idx = runner.state().objects[&p0_artifact]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::GainLife { .. }))
        .expect("the Food-granted sacrifice-for-life ability must be materialized");

    // Read the description BEFORE activation — the artifact leaves the
    // battlefield and the grant ends once it's sacrificed.
    let desc = runner.state().objects[&p0_artifact].abilities[idx]
        .description
        .clone();
    assert_eq!(
        desc.as_deref(),
        Some("{2}, {T}, Sacrifice ~: You gain 3 life."),
        "CR 700.7: the granted body must render its host-self-reference as ~, \
         never the granting Class's name"
    );

    let outcome = runner
        .activate(p0_artifact, idx)
        .pay_with(&[p0_artifact])
        .resolve();

    assert_eq!(
        outcome.life_delta(P0),
        3,
        "CR 119.3: activating the granted ability must gain exactly 3 life"
    );
    assert_eq!(
        outcome.zone_of(p0_artifact),
        Zone::Graveyard,
        "CR 701.21a: the sacrificed HOST artifact must go to the graveyard"
    );
    assert_eq!(
        outcome.zone_of(gourmand),
        Zone::Battlefield,
        "the granting Class must remain on the battlefield — it is not the \
         object the cost sacrifices"
    );
}

/// V13 (CR 716.3): the level-1 static is not class-level gated — it applies
/// "at all times" once the Class is on the battlefield, unlike the level-2
/// and level-3 abilities. The paired positive proves the class-level
/// machinery is actually live in this fixture: the level-2 lifegain trigger
/// on the SAME object DOES carry `TriggerCondition::ClassLevelGE { level: 2 }`.
#[test]
fn level_one_static_is_not_class_level_gated() {
    let (runner, gourmand, ..) = setup();

    let level1 = runner.state().objects[&gourmand]
        .base_static_definitions
        .iter()
        .find(|s| {
            s.modifications.iter().any(|m| {
                matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Food")
            })
        })
        .cloned()
        .expect("level-1 static must parse");
    assert_eq!(
        level1.condition,
        Some(StaticCondition::DuringYourTurn),
        "the level-1 static's condition must be exactly DuringYourTurn, with no \
         ClassLevelGE anywhere in it: {:?}",
        level1.condition
    );

    // Paired positive: the level-2 lifegain trigger DOES carry ClassLevelGE{2},
    // proving the Class pre-parser's level-sectioning is live in this fixture
    // (only reachable because `with_subtypes(["Class"])` ran BEFORE
    // `from_oracle_text` — B1).
    let level2_trigger_exists = runner.state().objects[&gourmand]
        .trigger_definitions
        .iter_unchecked()
        .map(engine::types::ability::TriggerEntry::definition)
        .any(|t| {
            t.mode == TriggerMode::LifeGained
                && matches!(
                    t.condition,
                    Some(TriggerCondition::ClassLevelGE { level: 2 })
                )
        });
    assert!(
        level2_trigger_exists,
        "the level-2 lifegain trigger must carry ClassLevelGE{{level: 2}}, proving \
         the class-level machinery is live in this fixture: {:?}",
        runner.state().objects[&gourmand]
            .trigger_definitions
            .iter_unchecked()
            .map(engine::types::ability::TriggerEntry::definition)
            .map(|t| (t.mode.clone(), t.condition.clone()))
            .collect::<Vec<_>>()
    );
}

/// CR 109.5 — the static's `artifacts you control` filter and its
/// `DuringYourTurn` window both bind to CONTROL, never to ownership.
///
/// Every other fixture in this file keeps `owner == controller`, so an
/// implementation that mistakenly filtered on `owner` would stay green
/// throughout. This test drives the real `Effect::GainControl` path
/// (`{T}: Gain control of target artifact.` activated through the runner, the
/// same production route as `gain_control_multi_target_6205.rs`) to force the
/// two apart: a P1-OWNED artifact becomes P0-CONTROLLED, ownership unchanged.
/// The grant must follow control.
#[test]
fn granted_food_follows_controller_not_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Owned by the opponent for the whole test; only its controller moves.
    let stolen = scenario
        .add_artifact_from_oracle(P1, "Owned By Opponent", "")
        .id();
    // Verbatim activated ability so the real `GainControl` parse + resolution
    // path runs, rather than a hand-mutated `controller` field.
    let thief = scenario
        .add_creature_from_oracle(
            P0,
            "Artifact Thief",
            1,
            1,
            "{T}: Gain control of target artifact.",
        )
        .id();
    // B1: subtype BEFORE oracle text (see this file's module doc). Bound but
    // unread: this test asserts on the artifact the static REACHES, and the
    // source only needs to be on the battlefield under P0's control for its
    // continuous effect to be collected (CR 611.3b).
    let _gourmand = scenario
        .add_creature(P0, "Gourmand's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(GOURMANDS_TALENT)
        .id();

    let mut runner = scenario.build();

    // Pre-state guard: the opponent really does control it right now, so the
    // post-steal assertions cannot pass vacuously.
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P1,
        "pre-state: the artifact must start under the opponent's control"
    );

    runner
        .activate(thief, 0)
        .target_objects(&[stolen])
        .resolve();

    // The divergence this test exists to create.
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P0,
        "CR 613.1b + CR 109.5: the activated GainControl applies a layer-2 \
         control-changing effect, so P0 becomes the controller"
    );
    assert_eq!(
        runner.state().objects[&stolen].owner,
        P1,
        "ownership must be untouched — this is what makes the next assertion \
         discriminate control from ownership (CR 108.3 / CR 109.5)"
    );

    // P0's turn: the grant must reach the artifact P0 now CONTROLS but does
    // not OWN. An `owner`-based filter fails here and nowhere else in the file.
    runner.state_mut().active_player = P0;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&stolen];
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "CR 109.5: the additive Food subtype must follow CONTROL, not ownership: {:?}",
        obj.card_types
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Artifact),
        "CR 205.1b: the stolen permanent must remain an Artifact: {:?}",
        obj.card_types
    );
    assert_eq!(
        obj.abilities.len(),
        1,
        "the granted sacrifice-for-life ability must follow control too: {:?}",
        obj.abilities
    );

    // And it switches off on the opponent's turn, still keyed to the SOURCE's
    // controller (P0) rather than to the artifact's owner (P1) — an
    // owner-keyed window would turn the grant ON here instead.
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&stolen];
    assert!(
        !obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "CR 102.1 + CR 109.5: the window follows the SOURCE's controller (P0), so on \
         P1's turn the grant is off even though P1 still OWNS the artifact: {:?}",
        obj.card_types
    );
    assert!(
        obj.abilities.is_empty(),
        "the granted ability must be gone on the opponent's turn: {:?}",
        obj.abilities
    );
}

/// CR 109.5 + CR 613.1b — the SOURCE side of the same binding: the window
/// follows the controller of the Class, not its owner.
///
/// `granted_food_follows_controller_not_owner` above diverges owner from
/// controller on the AFFECTED artifact, but leaves Gourmand's Talent itself
/// owned and controlled by P0 — so an implementation that read the SOURCE's
/// `owner` when evaluating `DuringYourTurn` would still pass it. This test
/// closes that axis: P1 OWNS the Class, P0 CONTROLS it (moved through the real
/// `Effect::GainControl` path), and the window must follow P0.
///
/// An owner-keyed window inverts every assertion below — it would switch the
/// grant on during P1's turn and off during P0's.
#[test]
fn window_follows_source_controller_not_source_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's own artifact — the grant recipient. Its control never moves, so the
    // only owner/controller divergence in this test is on the SOURCE.
    let p0_artifact = scenario
        .add_artifact_from_oracle(P0, "Vanilla Artifact", "")
        .id();
    let thief = scenario
        .add_creature_from_oracle(
            P0,
            "Enchantment Thief",
            1,
            1,
            "{T}: Gain control of target enchantment.",
        )
        .id();
    // OWNED BY P1. B1: subtype BEFORE oracle text (see this file's module doc).
    let gourmand = scenario
        .add_creature(P1, "Gourmand's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(GOURMANDS_TALENT)
        .id();

    let mut runner = scenario.build();

    // Pre-state guard: P1 really does control its own Class right now.
    assert_eq!(
        runner.state().objects[&gourmand].controller,
        P1,
        "pre-state: the Class must start under its owner's control"
    );

    runner
        .activate(thief, 0)
        .target_objects(&[gourmand])
        .resolve();

    assert_eq!(
        runner.state().objects[&gourmand].controller,
        P0,
        "CR 613.1b: the activated GainControl must move the Class to P0"
    );
    assert_eq!(
        runner.state().objects[&gourmand].owner,
        P1,
        "the Class must still be OWNED by P1 — that divergence is the whole point"
    );

    // P0's turn: P0 controls the Class, so the window is open and P0's artifact
    // is a Food. An owner-keyed window would be CLOSED here (owner is P1).
    runner.state_mut().active_player = P0;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&p0_artifact];
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "CR 109.5: 'your turn' is the CONTROLLER's turn (P0), even though P1 owns \
         the Class: {:?}",
        obj.card_types
    );
    assert_eq!(
        obj.abilities.len(),
        1,
        "the granted ability must be present on the controller's turn: {:?}",
        obj.abilities
    );

    // P1's turn: P1 OWNS the Class but no longer controls it, so the window is
    // shut. An owner-keyed window would be OPEN here — this is the assertion
    // that discriminates the two implementations.
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&p0_artifact];
    assert!(
        !obj.card_types.subtypes.iter().any(|s| s == "Food"),
        "CR 102.1 + CR 109.5: the window must be shut on the OWNER's turn once \
         control has moved away: {:?}",
        obj.card_types
    );
    assert!(
        obj.abilities.is_empty(),
        "the granted ability must be gone on the owner's turn: {:?}",
        obj.abilities
    );
}

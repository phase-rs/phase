//! Moon Girl and Devil Dinosaur — "Whenever you draw your second card each turn,
//! until end of turn, Moon Girl and Devil Dinosaur's base power and toughness
//! become 6/6 and they gain trample."
//!
//! This drives the REAL parse → trigger → stack → layer pipeline: Moon Girl is
//! built from Oracle text via the scenario harness (production synthesis path).
//! The second-card-each-turn trigger (`TriggerConstraint::NthDrawThisTurn { n: 2 }`)
//! fires off real `draw::resolve` events; the triggered ability resolves off the
//! stack, installing a continuous `GenericEffect` carrying `SetPower {6}` /
//! `SetToughness {6}` (Layer 7b, CR 613.4b) + `AddKeyword { Trample }` (Layer 6,
//! CR 613.1f) for `until end of turn` (CR 611.2a). After `evaluate_layers`, the
//! effective base P/T is 6/6 and Moon Girl has trample.
//!
//! THE BUG this discriminates: before the fix the possessive "~'s base power and
//! toughness become 6/6 and they gain trample" clause was left
//! `Effect::Unimplemented`, so the trigger resolved to a no-op. Assertion (a) —
//! base power AND base toughness both become 6/6 — and assertion (b) — Moon Girl
//! gains trample — both flip to failure if the parser change is reverted (no
//! GenericEffect is produced, so P/T stays 1/1 and there is no trample).
//! Assertion (c) — the first draw alone does NOT fire (P/T still base) —
//! discriminates the NthDrawThisTurn=2 gate from a fire-on-every-draw misparse.

use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::stack::resolve_top;
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const MOON_GIRL: &str = "Whenever you draw your second card each turn, until end of turn, Moon Girl and Devil Dinosaur's base power and toughness become 6/6 and they gain trample.\n\
Whenever an artifact you control enters, draw a card. This ability triggers only once each turn.";

/// Resolve one draw for P0 through the production `draw::resolve` seam, then
/// process the resulting triggers (the second draw queues the NthDraw trigger).
fn draw_one(runner: &mut GameRunner) {
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        P0,
    );
    let mut events = Vec::new();
    resolve_draw(runner.state_mut(), &ability, &mut events).expect("draw resolves");
    process_triggers(runner.state_mut(), &events);
}

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("has power"),
        obj.toughness.expect("has toughness"),
    )
}

#[test]
fn moon_girl_second_draw_sets_base_6_6_and_grants_trample() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Moon Girl (base 1/1), built from Oracle text through the real parse +
    // synthesis pipeline so the NthDrawThisTurn trigger is installed.
    let moon_girl = scenario
        .add_creature_from_oracle(P0, "Moon Girl and Devil Dinosaur", 1, 1, MOON_GIRL)
        .id();

    // Seed P0's library with cards to draw.
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }

    let mut runner = scenario.build();

    // Baseline: Moon Girl is a 1/1 with no trample.
    assert_eq!(
        effective_pt(&mut runner, moon_girl),
        (1, 1),
        "Moon Girl starts as a base 1/1"
    );
    assert!(!has_keyword(
        &runner.state().objects[&moon_girl],
        &Keyword::Trample
    ));

    // First draw of the turn: the NthDrawThisTurn=2 trigger must NOT fire yet.
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "first draw must not queue the second-draw trigger"
    );
    // (c) After only one draw, P/T is unchanged — discriminates the n=2 gate.
    assert_eq!(
        effective_pt(&mut runner, moon_girl),
        (1, 1),
        "the trigger gate is the SECOND draw — one draw leaves Moon Girl 1/1"
    );

    // Second draw: the trigger fires and goes on the stack.
    draw_one(&mut runner);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the second draw fires 'draw your second card each turn'"
    );

    // Resolve the triggered ability — installs the until-EOT GenericEffect.
    let mut events = Vec::new();
    resolve_top(runner.state_mut(), &mut events);

    // (a) Base power AND toughness both become 6/6 (CR 613.4b set effect).
    assert_eq!(
        effective_pt(&mut runner, moon_girl),
        (6, 6),
        "CR 613.4b: Moon Girl's base power and toughness become 6/6 on the second draw"
    );

    // (b) Moon Girl gains trample (CR 613.1f keyword grant).
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert!(
        has_keyword(&runner.state().objects[&moon_girl], &Keyword::Trample),
        "CR 613.1f + CR 702.19: Moon Girl gains trample on the second draw"
    );
}

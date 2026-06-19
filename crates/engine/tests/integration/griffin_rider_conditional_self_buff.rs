//! Griffin Rider — "As long as you control a Griffin creature, this creature
//! gets +3/+3 and has flying."
//!
//! Regression coverage for the **conditional ("as long as") static** building
//! block: a self-referential continuous effect gated on a game-state condition
//! that ALSO conjoins a P/T modification with a keyword grant. Axes:
//!   - **conditional gate** — the effect is active only while you control a
//!     Griffin (CR 604.1 static ability / CR 611.3a continuous effect),
//!   - **conjunction** — +3/+3 (Layer 7c) AND flying (Layer 6) together,
//!   - **gate toggling** — both effects deactivate when the Griffin leaves
//!     (CR 611.3a).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness and keyword set — a runtime
//! test, not an AST-shape test.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const GRIFFIN_RIDER: &str =
    "As long as you control a Griffin creature, this creature gets +3/+3 and has flying.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

#[test]
fn griffin_rider_buff_tracks_griffin_control_gate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a 2/2 carrying the conditional self-buff (real parse + synthesis
    // pipeline). It is NOT itself a Griffin.
    let rider = scenario
        .add_creature_from_oracle(P0, "Griffin Rider", 2, 2, GRIFFIN_RIDER)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();

    // A Griffin you control turns the gate ON.
    let griffin = scenario
        .add_creature(P0, "Griffin Token", 2, 2)
        .with_subtypes(vec!["Griffin"])
        .id();

    let mut runner = scenario.build();

    // Gate ON: CR 611.3a — base 2/2 + 3/3 = 5/5, and flying is granted.
    assert_eq!(
        effective_pt(&mut runner, rider),
        (5, 5),
        "gate ON: Griffin Rider is base 2/2 + 3/3 while you control a Griffin"
    );
    assert!(
        has_kw(&mut runner, rider, &Keyword::Flying),
        "gate ON: Griffin Rider gains flying while you control a Griffin"
    );

    // Gate OFF: remove the Griffin (CR 611.3a) — both the +3/+3 and flying end.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != griffin);
        state.objects.remove(&griffin);
    }
    assert_eq!(
        effective_pt(&mut runner, rider),
        (2, 2),
        "gate OFF: Griffin Rider returns to base 2/2 with no Griffin controlled"
    );
    assert!(
        !has_kw(&mut runner, rider, &Keyword::Flying),
        "gate OFF: Griffin Rider loses flying with no Griffin controlled"
    );
}

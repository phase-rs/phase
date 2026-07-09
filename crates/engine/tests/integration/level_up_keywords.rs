//! Regression coverage for the leveler keyword bug (CR 711): a leveler card must
//! NOT grant the keywords printed inside its {LEVEL} striations until it has the
//! required number of level counters.
//!
//! THE BUG: MTGJSON's `keywords` array lists every keyword printed anywhere on the
//! card, including those inside {LEVEL} blocks. That array seeds the runtime
//! object's `base_keywords`, so a freshly-cast leveler (0 level counters) carried
//! its level-gated keywords unconditionally — Student of Warfare had First strike
//! at level 0, Coralhelm Commander had Flying at level 0. CR 711.5: below the
//! lowest level, the creature has only its uppermost (base) P/T and its base
//! abilities; the {LEVEL} keywords are level-gated static abilities (CR 711.2a /
//! CR 711.2b), not base abilities (CR 711.4).
//!
//! THE FIX (`database::synthesis::strip_level_gated_keywords`, called from
//! `synthesize_level_up`): strip every keyword that is sourced from a level-gated
//! static out of the base `keywords` list, so the layer system grants those
//! keywords ONLY through the gated statics once the level threshold is met.
//!
//! These tests drive the real pipeline: build the leveler from Oracle text (the
//! scenario harness runs the same `synthesize_all` as production), then activate
//! the level-up ability the required number of times and read the *effective*
//! (post-layer) power/toughness and keywords. They are runtime tests, not AST
//! shape tests.
//!
//! WHY THESE FAIL ON main (the discrimination gate): on main, `face.keywords`
//! retains the {LEVEL}-printed keywords, so `base_keywords` carries them and they
//! survive layer evaluation at every level — including level 0. Every "level 0,
//! NO <keyword>" assertion below therefore fails on main (the keyword is present
//! ungated). The P/T assertions pass on main (P/T is driven by the gated
//! `SetPower`/`SetToughness` statics, which were already correct); only the
//! keyword assertions discriminate the fix.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Add one unit of `ty` mana to P0's pool (mirrors the Sheoldred's Edict harness —
/// deterministic payment without modelling lands).
fn add_mana(runner: &mut GameRunner, ty: ManaType) {
    let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
    runner.state_mut().players[0].mana_pool.add(unit);
}

/// Recompute layers and read the leveler's effective (post-layer) power/toughness.
/// CR 711.2a / CR 711.2b: the gated `SetPower`/`SetToughness` modifications apply
/// here once the level threshold is met; below it (CR 711.5) the base P/T stands.
fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("leveler has power"),
        obj.toughness.expect("leveler has toughness"),
    )
}

/// True iff the leveler currently has `keyword` after a fresh layer evaluation.
/// CR 711.2a / CR 711.2b: keywords inside a {LEVEL} striation are only present
/// once the level threshold is met.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// Activate the leveler's level-up ability once (ability index 0 — it is the only
/// synthesized activated ability). Funds the pool via `add_mana` first, then drives
/// the activation + resolution so a level counter lands (CR 702.87a).
fn level_up(runner: &mut GameRunner, id: ObjectId, fund: impl Fn(&mut GameRunner)) {
    fund(runner);
    runner.activate(id, 0).resolve();
}

/// Student of Warfare — base 1/1; "Level up {W}"; LEVEL 2-6 → 3/3 First strike;
/// LEVEL 7+ → 4/4 Double strike.
const STUDENT_OF_WARFARE: &str = "Level up {W}\n\
LEVEL 2-6\n\
3/3\n\
First strike\n\
LEVEL 7+\n\
4/4\n\
Double strike";

#[test]
fn student_of_warfare_first_strike_is_level_gated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_from_oracle(P0, "Student of Warfare", 1, 1, STUDENT_OF_WARFARE)
        .id();
    let mut runner = scenario.build();

    // CR 711.5 / CR 711.4: at level 0 the creature is its base 1/1 with NO
    // level-gated keywords. This is the assertion that fails on main (First strike
    // ungated in base_keywords).
    assert_eq!(effective_pt(&mut runner, id), (1, 1), "base P/T at level 0");
    assert!(
        !has_kw(&mut runner, id, &Keyword::FirstStrike),
        "CR 711.4: First strike is level-gated (LEVEL 2-6) — absent at level 0"
    );
    assert!(
        !has_kw(&mut runner, id, &Keyword::DoubleStrike),
        "CR 711.4: Double strike is level-gated (LEVEL 7+) — absent at level 0"
    );

    // Level up to 2: enter the LEVEL 2-6 band.
    for _ in 0..2 {
        level_up(&mut runner, id, |r| add_mana(r, ManaType::White));
    }

    // CR 711.2a: at 2 level counters → base 3/3, First strike, still NO Double strike.
    assert_eq!(effective_pt(&mut runner, id), (3, 3), "LEVEL 2-6 sets 3/3");
    assert!(
        has_kw(&mut runner, id, &Keyword::FirstStrike),
        "CR 711.2a: First strike granted within LEVEL 2-6"
    );
    assert!(
        !has_kw(&mut runner, id, &Keyword::DoubleStrike),
        "CR 711.2b: Double strike (LEVEL 7+) still absent below 7 counters"
    );

    // Level up to 7: enter the LEVEL 7+ band (5 more activations).
    for _ in 0..5 {
        level_up(&mut runner, id, |r| add_mana(r, ManaType::White));
    }

    // CR 711.2b: at 7 level counters → base 4/4, Double strike.
    assert_eq!(effective_pt(&mut runner, id), (4, 4), "LEVEL 7+ sets 4/4");
    assert!(
        has_kw(&mut runner, id, &Keyword::DoubleStrike),
        "CR 711.2b: Double strike granted within LEVEL 7+"
    );
}

/// Coralhelm Commander — base 2/2; "Level up {1}{U}"; LEVEL 2-3 → 4/4 Flying;
/// LEVEL 4+ → 6/6 Flying. (Both bands grant Flying; the gate is what is under test.)
const CORALHELM_COMMANDER: &str = "Level up {1}{U}\n\
LEVEL 2-3\n\
4/4\n\
Flying\n\
LEVEL 4+\n\
6/6\n\
Flying";

#[test]
fn coralhelm_commander_flying_is_level_gated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_from_oracle(P0, "Coralhelm Commander", 2, 2, CORALHELM_COMMANDER)
        .id();
    let mut runner = scenario.build();

    // CR 711.5 / CR 711.4: base 2/2 with NO Flying at level 0 (fails on main).
    assert_eq!(effective_pt(&mut runner, id), (2, 2), "base P/T at level 0");
    assert!(
        !has_kw(&mut runner, id, &Keyword::Flying),
        "CR 711.4: Flying is level-gated (LEVEL 2-3 / 4+) — absent at level 0"
    );

    // Pay {1}{U} per level — one Colorless for the generic, one Blue.
    let pay_blue = |r: &mut GameRunner| {
        add_mana(r, ManaType::Colorless);
        add_mana(r, ManaType::Blue);
    };

    // Level up to 2: enter LEVEL 2-3.
    for _ in 0..2 {
        level_up(&mut runner, id, pay_blue);
    }

    // CR 711.2a: at 2 level counters → 4/4, Flying.
    assert_eq!(effective_pt(&mut runner, id), (4, 4), "LEVEL 2-3 sets 4/4");
    assert!(
        has_kw(&mut runner, id, &Keyword::Flying),
        "CR 711.2a: Flying granted within LEVEL 2-3"
    );

    // Level up to 4: enter LEVEL 4+ (2 more activations).
    for _ in 0..2 {
        level_up(&mut runner, id, pay_blue);
    }

    // CR 711.2b: at 4 level counters → 6/6, still Flying.
    assert_eq!(effective_pt(&mut runner, id), (6, 6), "LEVEL 4+ sets 6/6");
    assert!(
        has_kw(&mut runner, id, &Keyword::Flying),
        "CR 711.2b: Flying granted within LEVEL 4+"
    );
}

/// Hada Spy Patrol — base 1/1; "Level up {2}{U}"; LEVEL 1-2 → 2/2 can't be blocked;
/// LEVEL 3+ → 3/3 Shroud + can't be blocked (#2412).
const HADA_SPY_PATROL: &str =
    "Level up {2}{U} ({2}{U}: Put a level counter on this. Level up only as a sorcery.)\n\
LEVEL 1-2\n\
2/2\n\
This creature can't be blocked.\n\
LEVEL 3+\n\
3/3\n\
Shroud (This creature can't be the target of spells or abilities.)\n\
This creature can't be blocked.";

#[test]
fn hada_spy_patrol_shroud_is_level_gated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_from_oracle(P0, "Hada Spy Patrol", 1, 1, HADA_SPY_PATROL)
        .id();
    let mut runner = scenario.build();

    assert_eq!(effective_pt(&mut runner, id), (1, 1), "base P/T at level 0");
    assert!(
        !has_kw(&mut runner, id, &Keyword::Shroud),
        "CR 711.4: Shroud is level-gated (LEVEL 3+) — absent at level 0"
    );

    let pay_blue = |r: &mut GameRunner| {
        add_mana(r, ManaType::Colorless);
        add_mana(r, ManaType::Colorless);
        add_mana(r, ManaType::Blue);
    };

    for _ in 0..3 {
        level_up(&mut runner, id, pay_blue);
    }

    assert_eq!(effective_pt(&mut runner, id), (3, 3), "LEVEL 3+ sets 3/3");
    assert!(
        has_kw(&mut runner, id, &Keyword::Shroud),
        "CR 711.2b: Shroud granted at 3+ level counters"
    );
}

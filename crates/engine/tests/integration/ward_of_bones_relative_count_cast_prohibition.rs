//! Ward of Bones — production-path proof that the *relative-count* cast
//! prohibition is enforced PER TYPE, not collapsed onto one shared count.
//!
//! Oracle (verbatim first line): "Each opponent who controls more creatures than
//! you can't cast creature spells. The same is true for artifacts and
//! enchantments."
//!
//! CR 101.2 + CR 109.4 + CR 601.3a: each type is an INDEPENDENT prohibition — an
//! opponent controlling more `<T>` than you can't cast `<T>` spells, gated on that
//! type's OWN count. The parser now emits one `CantBeCast` static per type. These
//! tests drive the REAL pre-payment gate (`can_cast_object_now` →
//! `is_blocked_by_cant_be_cast_for`) and prove the cast is rejected ONLY for the
//! type whose count comparison holds. Under the previous single-static model
//! (one `Or[creature,artifact,enchantment]` gated on the creature count) the
//! artifact/enchantment "allowed" assertions FAIL — an opponent with more
//! creatures would be wrongly barred from every spell type. So each test is its
//! own revert-probe. The "allowed" assertions also reach-guard the "blocked"
//! ones: they prove a {0}-cost sorcery-speed spell is otherwise castable now, so
//! the block is the prohibition, not a timing/mana artifact.

use engine::game::casting::can_cast_object_now;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

// Verbatim first line (Scryfall). The second line ("controls more lands than you
// can't play lands") is a separate land-play prohibition, inert for these cast
// tests; parsing the whole first line proves the three cast statics are extracted
// from the real multi-clause sentence.
const WARD_OF_BONES_CAST_LINE: &str =
    "Each opponent who controls more creatures than you can't cast creature spells. \
     The same is true for artifacts and enchantments.";

fn zero_creature_spell(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
) -> ObjectId {
    scenario
        .add_creature_to_hand(owner, "Test Creature Spell", 1, 1)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn zero_artifact_spell(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
) -> ObjectId {
    scenario
        .add_creature_to_hand(owner, "Test Artifact Spell", 0, 0)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn zero_enchantment_spell(
    scenario: &mut GameScenario,
    owner: engine::types::player::PlayerId,
) -> ObjectId {
    scenario
        .add_creature_to_hand(owner, "Test Enchantment Spell", 0, 0)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// P1 controls MORE creatures than P0 (2 vs 0) but NOT more artifacts (0 vs P0's
/// Ward of Bones) nor more enchantments (0 vs 0). Only P1's CREATURE spell is
/// prohibited; its artifact and enchantment spells stay castable.
#[test]
fn more_creatures_blocks_only_creature_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ward of Bones is an artifact P0 controls (so P0's artifact count is 1).
    scenario
        .add_creature(P0, "Ward of Bones", 0, 0)
        .as_artifact()
        .from_oracle_text(WARD_OF_BONES_CAST_LINE);

    // P1 controls two creatures — strictly more than P0's zero.
    scenario.add_creature(P1, "P1 Bear A", 2, 2);
    scenario.add_creature(P1, "P1 Bear B", 2, 2);

    let creature_spell = zero_creature_spell(&mut scenario, P1);
    let artifact_spell = zero_artifact_spell(&mut scenario, P1);
    let enchantment_spell = zero_enchantment_spell(&mut scenario, P1);

    let mut runner = scenario.build();
    // Sorcery-speed casts require it to be P1's main phase with an empty stack.
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // CR 601.3a: P1 controls more creatures → creature spells prohibited.
    assert!(
        !can_cast_object_now(runner.state(), P1, creature_spell),
        "P1 controls more creatures than you → creature spell must be prohibited"
    );
    // Per-type independence + reach-guard: P1 does NOT control more artifacts
    // (0 vs Ward of Bones' 1), so the artifact spell stays castable. FAILS under
    // the old single-Or-gated-on-creature-count model.
    assert!(
        can_cast_object_now(runner.state(), P1, artifact_spell),
        "P1 does NOT control more artifacts than you → artifact spell must stay castable \
         (revert-probe for the collapsed single-count model)"
    );
    assert!(
        can_cast_object_now(runner.state(), P1, enchantment_spell),
        "P1 does NOT control more enchantments than you → enchantment spell must stay castable"
    );
}

/// The inverse: P1 controls MORE artifacts than P0 (2 vs Ward of Bones' 1) but NOT
/// more creatures (0 vs 0). Only P1's ARTIFACT spell is prohibited; its creature
/// spell stays castable. Together with the test above this pins the per-type
/// discrimination in BOTH directions (the reviewer's creature-vs-artifact case).
#[test]
fn more_artifacts_blocks_only_artifact_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Ward of Bones", 0, 0)
        .as_artifact()
        .from_oracle_text(WARD_OF_BONES_CAST_LINE);

    // P1 controls two artifacts — strictly more than P0's one (Ward of Bones).
    scenario.add_creature(P1, "P1 Relic A", 0, 0).as_artifact();
    scenario.add_creature(P1, "P1 Relic B", 0, 0).as_artifact();

    let creature_spell = zero_creature_spell(&mut scenario, P1);
    let artifact_spell = zero_artifact_spell(&mut scenario, P1);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // CR 601.3a: P1 controls more artifacts → artifact spells prohibited.
    assert!(
        !can_cast_object_now(runner.state(), P1, artifact_spell),
        "P1 controls more artifacts than you → artifact spell must be prohibited"
    );
    // Per-type independence + reach-guard: P1 does NOT control more creatures
    // (0 vs 0), so the creature spell stays castable — the SAME creature spell the
    // first test proves is blocked when P1 has more creatures.
    assert!(
        can_cast_object_now(runner.state(), P1, creature_spell),
        "P1 does NOT control more creatures than you → creature spell must stay castable \
         (revert-probe for the collapsed single-count model)"
    );
}

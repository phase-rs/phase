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
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

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

/// The ENCHANTMENT arm of the per-type independence. P1 controls MORE enchantments
/// than P0 (1 vs 0) but NOT more creatures (0 vs 0) nor more artifacts (0 vs Ward
/// of Bones' 1). Only P1's ENCHANTMENT spell is prohibited; its creature and
/// artifact spells stay castable. The existing tests never proved an enchantment
/// cast is BLOCKED — this closes that gap and, together with the creature/artifact
/// tests, pins the enchantment count as its OWN independent gate.
#[test]
fn more_enchantments_blocks_only_enchantment_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ward of Bones is an artifact P0 controls (P0 artifacts = 1, creatures = 0,
    // enchantments = 0).
    scenario
        .add_creature(P0, "Ward of Bones", 0, 0)
        .as_artifact()
        .from_oracle_text(WARD_OF_BONES_CAST_LINE);

    // P1 controls a single enchantment permanent — strictly more than P0's zero.
    // `as_enchantment` strips the creature type, so this counts ONLY as an
    // enchantment (not toward P1's creature count).
    scenario
        .add_creature(P1, "P1 Enchantment", 0, 0)
        .as_enchantment();

    let creature_spell = zero_creature_spell(&mut scenario, P1);
    let artifact_spell = zero_artifact_spell(&mut scenario, P1);
    let enchantment_spell = zero_enchantment_spell(&mut scenario, P1);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // CR 601.3a: P1 controls more enchantments → enchantment spells prohibited.
    assert!(
        !can_cast_object_now(runner.state(), P1, enchantment_spell),
        "P1 controls more enchantments than you → enchantment spell must be prohibited"
    );
    // Per-type independence + reach-guard: P1 does NOT control more creatures
    // (0 vs 0) nor more artifacts (0 vs Ward of Bones' 1), so those spells stay
    // castable. FAILS under any model that collapses the enchantment gate onto a
    // shared (creature) count.
    assert!(
        can_cast_object_now(runner.state(), P1, creature_spell),
        "P1 does NOT control more creatures than you → creature spell must stay castable"
    );
    assert!(
        can_cast_object_now(runner.state(), P1, artifact_spell),
        "P1 does NOT control more artifacts than you → artifact spell must stay castable"
    );
}

/// Boundary case: P1 controls the SAME number of creatures as P0 (1 vs 1), not
/// STRICTLY more. "More than" is a strict inequality (`Comparator::GT`); the
/// prohibition must not fire on a tie. This is the one case the per-type tests
/// above never probe (they only ever use a strict count difference), so it's the
/// discriminating regression guard against the comparator silently regressing to
/// `GE` (which would wrongly block casts whenever the counts are merely equal).
#[test]
fn equal_creature_count_does_not_block_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Ward of Bones", 0, 0)
        .as_artifact()
        .from_oracle_text(WARD_OF_BONES_CAST_LINE);
    // P0 controls one creature — the threshold P1 must EXCEED, not merely meet.
    scenario.add_creature(P0, "P0 Bear", 2, 2);
    // P1 controls exactly one creature: equal to P0's count, not more.
    scenario.add_creature(P1, "P1 Bear", 2, 2);

    let creature_spell = zero_creature_spell(&mut scenario, P1);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Equal counts are not "more than" → the prohibition must not apply. FAILS
    // if `Comparator::GT` ever regresses to `GE`.
    assert!(
        can_cast_object_now(runner.state(), P1, creature_spell),
        "P1 controls the SAME number of creatures as you (1 vs 1), not more → \
         the creature spell must stay castable (regression guard for GT vs GE)"
    );
}

// ---------------------------------------------------------------------------
// Two-Headed Giant: "each opponent" must not treat a TEAMMATE as an opponent
// on the CAST seam (sibling of the land-play fix).
//
// CR 102.2 / CR 102.3 + CR 810.1: In a multiplayer team game a player's
// opponents are only players NOT on their team. Ward of Bones' three per-type
// prohibitions lower to `CantBeCast { who: ProhibitionScope::Opponents }`; the
// cast gate consumes them through `casting_prohibition_scope_matches` →
// `prohibition_scope_matches_player`, whose `Opponents` arm previously used the
// naive inequality `player != source_obj.controller`. In Two-Headed Giant, P0
// and P1 are TEAMMATES with different ids, so that inequality wrongly barred a
// teammate from CASTING. Routing the arm through the team-aware `is_opponent`
// authority fixes every `CantBeCast`/Opponents static; in a two-player game
// `is_opponent` reduces to `!=`, so the tests above are unchanged.
// ---------------------------------------------------------------------------

/// Player 2 (opposing team, seat 2). Under the Two-Headed Giant
/// `FixedTeams { team_size: 2 }` topology, team A = {P0, P1}, team B = {P2, P3}.
const P2: PlayerId = PlayerId(2);

/// Build a 4-player Two-Headed Giant board (team A = {P0, P1}, team B =
/// {P2, P3}). P0 controls Ward of Bones (an artifact) plus one creature, so
/// P0's creature count is 1 and artifact count is 1. Teammate P1 and opponent
/// P2 each control 3 creatures — strictly MORE than P0's 1 — so Ward's "controls
/// more creatures than you" per-player predicate holds for BOTH; the only
/// difference between them is team membership. Each of P1 and P2 holds a {0}
/// creature spell (the prohibition's subject) and a {0} noncreature-artifact
/// spell (a reach-guard: neither controls more artifacts than P0, so the
/// artifact prohibition never fires and its castability proves the sorcery-speed
/// window and mana are otherwise clear). Returns the runner (switched to the
/// Two-Headed Giant topology) plus P1's and P2's creature and artifact spell ids.
fn two_hg_ward_cast_scenario() -> (
    engine::game::scenario::GameRunner,
    ObjectId, // P1 creature spell
    ObjectId, // P1 artifact spell
    ObjectId, // P2 creature spell
    ObjectId, // P2 artifact spell
) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Ward of Bones is an artifact P0 controls (P0 artifacts = 1, creatures = 0
    // from Ward itself). Parse the full first line so all three CantBeCast
    // statics exist.
    scenario
        .add_creature(P0, "Ward of Bones", 0, 0)
        .as_artifact()
        .from_oracle_text(WARD_OF_BONES_CAST_LINE);

    // P0 controls exactly one creature → the relative-count threshold is 1.
    scenario.add_creature(P0, "P0 Bear", 2, 2);

    // Teammate P1 and opponent P2: 3 creatures each — strictly MORE than P0's 1.
    for i in 0..3 {
        scenario.add_creature(P1, &format!("P1 Bear {i}"), 2, 2);
        scenario.add_creature(P2, &format!("P2 Bear {i}"), 2, 2);
    }

    let p1_creature = zero_creature_spell(&mut scenario, P1);
    let p1_artifact = zero_artifact_spell(&mut scenario, P1);
    let p2_creature = zero_creature_spell(&mut scenario, P2);
    let p2_artifact = zero_artifact_spell(&mut scenario, P2);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        // CR 810.1 + CR 102.3: switch to the Two-Headed Giant topology so P0/P1
        // are teammates and P2/P3 are the opposing team. `is_opponent` reads
        // `format_config.topology()`, so this is the single field that makes the
        // team relationships real for the runtime cast gate.
        state.format_config = FormatConfig::two_headed_giant();
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    (runner, p1_creature, p1_artifact, p2_creature, p2_artifact)
}

/// DISCRIMINATING + positive-control + reach-guard on the CAST seam.
///
/// Both the teammate P1 and the opponent P2 control MORE creatures than Ward's
/// controller P0 (3 vs 1), so Ward's per-player creature predicate holds for
/// BOTH. The ONLY axis separating them is team membership — exactly what the
/// team-aware `is_opponent` fix discriminates on. The prohibition-scope check
/// (`prohibition_scope_matches_player`) is turn-INDEPENDENT — it reads only
/// controller/team relationships — so each caster is made the active player when
/// its own cast is probed (exactly as the two-player cast tests above do). This
/// satisfies the CR 307.1 sorcery-speed active-player timing gate and isolates
/// the pure opponent-scope determination the fix touches. (The separate question
/// of whether a NON-active teammate may cast a sorcery-speed spell under shared
/// team turns lives in `check_spell_timing` and is out of scope for this fix.)
///
/// - Teammate P1 CAN cast a creature spell (DISCRIMINATING): under the old
///   `player != source_obj.controller` arm, P1 != P0 made P1 an "opponent", the
///   creature predicate held (3 > 1), and the cast was wrongly blocked. The fix
///   recognizes P1 as a teammate (`is_opponent(P0, P1) == false`), so the static
///   never applies. Revert-probe: reverting the arm to `!=` flips this assertion
///   to failure (verified locally — reverted → this assertion fails → restored).
/// - Opposing P2 CANNOT cast a creature spell (positive control): P2 is on the
///   other team, `is_opponent(P0, P2)` holds, the predicate fires, and the cast
///   stays prohibited — the fix narrows "opponent" to the other team WITHOUT
///   disabling the prohibition for true opponents.
/// - Reach-guards: each player's {0} noncreature-artifact spell is castable now
///   (neither controls more artifacts than P0, so the artifact prohibition never
///   fires), proving the creature-spell block is the prohibition itself and not a
///   timing or mana artifact.
#[test]
fn two_headed_giant_teammate_can_cast_but_true_opponent_cannot() {
    let (mut runner, p1_creature, p1_artifact, p2_creature, p2_artifact) =
        two_hg_ward_cast_scenario();

    // --- Probe teammate P1's own casts (P1 active). CR 307.1: making P1 the
    // active player satisfies the sorcery-speed timing gate; the opponent-scope
    // determination the fix touches is turn-independent, so team membership
    // (P0/P1 same team) is the only axis in play. ---
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // DISCRIMINATING: teammate P1 controls more creatures than P0 (3 > 1) but is
    // NOT an opponent → the creature prohibition must not apply. FAILS under the
    // old `!=` opponent check.
    assert!(
        can_cast_object_now(runner.state(), P1, p1_creature),
        "P1 is P0's TEAMMATE in Two-Headed Giant, not an opponent → Ward of Bones \
         must NOT block P1's creature spell even though P1 controls more creatures \
         than P0 (revert-probe for the naive `!=` opponent check)"
    );
    // Reach-guard for P1: the noncreature-artifact spell is castable (P1 controls
    // 0 artifacts, not more than P0's 1), proving P1's sorcery-speed timing and
    // mana are clear and the creature-cast permission above is genuine.
    assert!(
        can_cast_object_now(runner.state(), P1, p1_artifact),
        "P1 does NOT control more artifacts than P0 → the artifact prohibition never \
         fires; the spell must be castable (reach-guard: timing/mana are clear)"
    );

    // --- Probe true opponent P2's own casts (P2 active), satisfying the same
    // sorcery-speed timing gate for P2's creature/artifact spells. ---
    runner.state_mut().active_player = P2;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Positive control: true opponent P2 controls more creatures than P0 (3 > 1)
    // → the creature prohibition still fires for a real opponent.
    assert!(
        !can_cast_object_now(runner.state(), P2, p2_creature),
        "P2 is a true opponent controlling more creatures than P0 → Ward of Bones \
         must still block P2's creature spell"
    );
    // Reach-guard for P2: the noncreature-artifact spell IS castable now (P2
    // controls 0 artifacts, not more than P0's 1), proving P2's creature-spell
    // block above is the creature prohibition itself — not a sorcery-speed-window,
    // priority, or mana artifact that would suppress every one of P2's spells.
    assert!(
        can_cast_object_now(runner.state(), P2, p2_artifact),
        "P2 does NOT control more artifacts than P0 → the artifact prohibition never \
         fires; the spell must be castable (reach-guard: P2's block is the creature \
         prohibition, not a timing/mana artifact)"
    );
}

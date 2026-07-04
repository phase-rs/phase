//! Runtime coverage for the effect-level "becomes saddled" instruction
//! (`Effect::BecomeSaddled`, CR 702.171b) — distinct from the Saddle keyword's
//! activated ability (CR 702.171a), which is paid by tapping creatures.
//!
//! CR 702.171b: "Saddled is a designation ... Only permanents can be or become
//! saddled. Once a permanent has become saddled, it stays saddled until the end
//! of the turn or it leaves the battlefield."
//!
//! These tests drive the REAL pipeline (`GameAction::ActivateAbility` /
//! cast → stack → resolve via the `GameScenario`/`GameRunner` harness), not the
//! parsed AST. Each asserts the saddled designation actually lands on game
//! state and would FLIP if the `BecomeSaddled` resolver were reverted to the
//! parser's `Unimplemented` no-op:
//!   - Guidelight Matrix: activating `{2},{T}: Target Mount you control becomes
//!     saddled` sets `is_saddled = true` on the chosen Mount. Revert the
//!     resolver → the effect no-ops → the `is_saddled` assertion fails.
//!   - Kolodin: a Mount entering under your control fires the trigger and
//!     becomes saddled; a non-Mount entering does NOT (the trigger filter +
//!     resolver discriminate the positive from the negative case).

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const GUIDELIGHT_MATRIX: &str = "When this artifact enters, draw a card.\n\
{2}, {T}: Target Mount you control becomes saddled until end of turn. Activate only as a sorcery.\n\
{2}, {T}: Target Vehicle you control becomes an artifact creature until end of turn.";

const KOLODIN: &str = "Mounts and Vehicles you control have haste.\n\
Whenever a Mount you control enters, it becomes saddled until end of turn.\n\
Whenever a Vehicle you control enters, it becomes an artifact creature until end of turn.";

/// CR 702.171b: activating Guidelight Matrix's `{2},{T}: Target Mount you
/// control becomes saddled` marks the targeted Mount saddled through the real
/// activation → resolution pipeline.
///
/// DISCRIMINATION: the only thing that sets `is_saddled` here is the
/// `Effect::BecomeSaddled` resolver running on the chosen Mount. Revert the
/// resolver (or its parser arm) and the line is `Effect::Unimplemented`, which
/// resolves to a no-op — `is_saddled` stays false and this assertion fails.
#[test]
fn guidelight_matrix_saddles_target_mount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Guidelight Matrix on the battlefield, parsed from real Oracle text.
    let matrix = scenario
        .add_creature_from_oracle(P0, "Guidelight Matrix", 0, 1, GUIDELIGHT_MATRIX)
        .id();

    // A Mount the controller owns — the legal target of the saddle ability.
    let mount = {
        let mut b = scenario.add_creature(P0, "Test Mount", 0, 4);
        b.with_subtypes(vec!["Mount"]);
        b.id()
    };

    // Pay {2} from the pool; {T} is the source's own tap.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&mount].is_saddled,
        "precondition: the Mount is not saddled before the ability resolves"
    );

    // Locate the `{2},{T}: ... becomes saddled` ability by its effect.
    let saddle_idx = runner.state().objects[&matrix]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::BecomeSaddled { .. }))
        .expect("Guidelight Matrix must have a BecomeSaddled activated ability");

    runner
        .activate(matrix, saddle_idx)
        .target_object(mount)
        .resolve();

    // CR 702.171b: the designation lands on the chosen Mount.
    assert!(
        runner.state().objects[&mount].is_saddled,
        "the targeted Mount must become saddled after the ability resolves"
    );
    // CR 702.171c: an effect-granted saddle records no saddling creatures.
    assert!(
        runner.state().objects[&mount].saddled_by.is_empty(),
        "an effect-granted saddle must not record any saddling creatures"
    );
}

/// CR 702.171b + CR 603.2: a Mount entering under Kolodin's control fires
/// "Whenever a Mount you control enters, it becomes saddled until end of turn"
/// and the entering Mount becomes saddled — driven through the cast → ETB →
/// trigger → resolve pipeline.
///
/// DISCRIMINATION: removing the `BecomeSaddled` resolver makes the trigger
/// effect a no-op `Unimplemented`, leaving the entering Mount unsaddled and
/// flipping the positive assertion.
#[test]
fn kolodin_saddles_entering_mount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Kolodin on the battlefield with its parsed Mount-enters trigger.
    scenario.add_creature_from_oracle(P0, "Kolodin, Triumph Caster", 2, 4, KOLODIN);

    // A Mount in hand to cast (scenario cards have ManaCost::zero()).
    let mount = {
        let mut b = scenario.add_creature_to_hand(P0, "New Mount", 1, 1);
        b.with_subtypes(vec!["Mount"]);
        b.id()
    };

    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&mount].is_saddled,
        "precondition: the Mount is not saddled while still in hand"
    );

    runner.cast(mount).resolve();

    // CR 702.171b: the entering Mount becomes saddled via the resolved trigger.
    assert!(
        runner.state().objects[&mount].is_saddled,
        "a Mount entering under Kolodin's control must become saddled"
    );
}

/// SIBLING / NEGATIVE: a non-Mount creature entering under Kolodin's control
/// must NOT become saddled — the trigger filter (`Subtype("Mount")`) gates the
/// `BecomeSaddled` effect, so a plain creature entering is unaffected. This
/// keeps the positive test honest: the saddle came from the Mount filter, not
/// from every ETB.
#[test]
fn kolodin_does_not_saddle_entering_non_mount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Kolodin, Triumph Caster", 2, 4, KOLODIN);

    // A non-Mount creature in hand.
    let bear = scenario.add_creature_to_hand(P0, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.cast(bear).resolve();

    assert!(
        !runner.state().objects[&bear].is_saddled,
        "a non-Mount entering under Kolodin must not become saddled"
    );
}

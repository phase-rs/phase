//! Backlog root cause 1 — "relative-clause / filter restriction on target dropped".
//!
//! Culling Scales: "At the beginning of your upkeep, destroy target nonland
//! permanent with the lowest mana value."
//!
//! A postnominal superlative qualifier with no trailing `among <set>` clause was
//! silently dropped by the target grammar, so the emitted filter was
//! `Typed { type_filters: [Permanent, Non(Land)], properties: [] }` — zero
//! `Effect::Unimplemented`, zero parse warnings, and the trigger could destroy
//! ANY nonland permanent rather than only a lowest-mana-value one.
//!
//! CR 109.2: a description with no zone clause and no "card" means permanents on
//! the battlefield, so the ranked population is the ENCLOSING noun phrase —
//! every nonland permanent. CR 601.2c: the controller announces one legal target,
//! and because the comparison is `EQ` against the population's minimum, EVERY
//! permanent tied for lowest is legal.
//!
//! This test drives the real trigger pipeline and asserts on the engine's own
//! `legal_targets` at `WaitingFor::TriggerTargetSelection` — the target-legality
//! boundary (CR 608.2b). Reverting the parser change makes the higher-mana-value
//! permanents legal again and the assertions fail.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

/// Culling Scales, verbatim (reminder text included — it is stripped by the parser).
const CULLING_SCALES: &str = "At the beginning of your upkeep, destroy target nonland permanent with the lowest mana value. (If two or more permanents are tied for lowest, target any one of them.)";

/// Drive forward until the engine pauses on a trigger target selection, passing
/// priority and declining combat. Mirrors
/// `magus_of_the_abyss_scoped_chooser.rs::advance_to_trigger_target_selection`.
fn advance_to_trigger_target_selection(runner: &mut GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DiscardToHandSize {
                count, ref cards, ..
            } => {
                let chosen: Vec<_> = cards.iter().take(*count).copied().collect();
                if runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// The engine's announced legal targets for the paused trigger, as object ids.
fn legal_target_ids(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter())
            .filter_map(|t| match t {
                engine::types::ability::TargetRef::Object(id) => Some(*id),
                _ => None,
            })
            .collect(),
        other => panic!("expected TriggerTargetSelection, got {other:?}"),
    }
}

/// CR 109.2 + CR 601.2c + CR 608.2b: only the lowest-mana-value nonland permanent
/// is a legal target. The two higher-cost permanents must be excluded, and the
/// Land must be excluded by the type conjunction.
#[test]
fn culling_scales_offers_only_the_lowest_mana_value_nonland_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Stock libraries so no one decks out before P0's next upkeep (CR 704.5b).
    let deck = ["Forest"; 12];
    scenario.with_library_top(P0, &deck);
    scenario.with_library_top(P1, &deck);

    // The Scales itself is MV 3 and is a nonland permanent, so it is inside its
    // own ranked population — but not the minimum here.
    scenario
        .add_creature_from_oracle(P0, "Culling Scales", 1, 1, CULLING_SCALES)
        .with_mana_cost(ManaCost::generic(3));

    // FOOT-GUN: add_creature does not set mana_cost, so every fixture permanent
    // needs an explicit one or they all tie at MV 0 and the test proves nothing.
    // Deliberate TIE at the population minimum. With a single legal target the
    // engine auto-targets and never surfaces `TriggerTargetSelection`, so a tie is
    // what makes the legal-set assertion observable at all (CR 601.2c: every
    // permanent tied for lowest is legal, per the card's own reminder text).
    let cheap = scenario
        .add_creature(P1, "Cheap Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let cheap_twin = scenario
        .add_creature(P0, "Cheap Twin", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mid = scenario
        .add_creature(P1, "Mid Bear", 3, 3)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let dear = scenario
        .add_creature(P0, "Dear Bear", 5, 5)
        .with_mana_cost(ManaCost::generic(6))
        .id();

    // A Land must be excluded by the `Non(Land)` leg regardless of its mana value.
    // It is given MV 0 — BELOW the tie — so the exclusion can only come from the
    // type conjunction, never from losing the mana-value comparison.
    let land = scenario.add_basic_land(P0, ManaColor::Green);

    let mut runner = scenario.build();

    advance_to_trigger_target_selection(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the upkeep trigger must pause for target selection, got {:?}",
        runner.state().waiting_for
    );

    let legal = legal_target_ids(&runner);
    // Reach-guard: the trigger really did offer targets, so the exclusions below
    // cannot pass vacuously on an empty slot.
    assert!(
        !legal.is_empty(),
        "reach-guard: the trigger must offer at least one legal target"
    );
    assert!(
        legal.contains(&cheap) && legal.contains(&cheap_twin),
        "both MV 1 permanents tie for the population minimum and must be legal; legal={legal:?}"
    );
    assert!(
        !legal.contains(&mid),
        "MV 4 is not the lowest — reverting the parser fix makes this legal again"
    );
    assert!(
        !legal.contains(&dear),
        "MV 6 is not the lowest — reverting the parser fix makes this legal again"
    );
    assert!(
        !legal.contains(&land),
        "a Land at MV 0 is below the tie yet must still be excluded by the \
         Non(Land) leg of the noun phrase; legal={legal:?}"
    );
}

/// Culling Scales' own shape carries no relative clause, so this row covers the
/// building block one step out: a MULTI-type trailing relative clause, where the
/// noun phrase spreads into one `Or` leg per type (maintainer review on PR #6789).
///
/// CR 109.2: the ranked population is the candidate set — here "permanents that are
/// artifacts or creatures" — expressed as a single conjunctive `TypeFilter::AnyOf`
/// (`base ∧ (A ∨ B) == (base ∧ A) ∪ (base ∧ B)`), and evaluated at runtime by
/// `type_filter_matches` (`game/filter.rs`).
///
/// The Land is the discriminator that separates the two failure modes this test
/// must tell apart:
///   * aggregate dropped entirely (the pre-fix bug) → the MV 5 creature is legal;
///   * aggregate present but ranked over `[Permanent]` only (the population/candidate
///     mismatch) → the Land's MV 0 sets the bar, no artifact or creature matches it,
///     so there is no legal target at all and the trigger never pauses for selection.
///
/// Both modes were confirmed to fail by patching them in and re-running.
#[test]
fn multi_type_relative_clause_ranks_over_the_disjunctive_population_at_runtime() {
    const MULTI_TYPE: &str = "At the beginning of your upkeep, destroy target permanent \
                              with the lowest mana value that's an artifact or creature.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let deck = ["Forest"; 12];
    scenario.with_library_top(P0, &deck);
    scenario.with_library_top(P1, &deck);

    // MV 3, and a creature — so inside its own population, but not the minimum.
    scenario
        .add_creature_from_oracle(P0, "Culling Scales", 1, 1, MULTI_TYPE)
        .with_mana_cost(ManaCost::generic(3));

    // Deliberate TIE at the population minimum, one member per `Or` leg: a
    // non-creature artifact and a non-artifact creature. A tie is required or the
    // engine auto-targets the sole legal object and never pauses.
    let artifact = scenario
        .add_creature(P1, "Cheap Relic", 1, 1)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let creature = scenario
        .add_creature(P0, "Cheap Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let expensive = scenario
        .add_creature(P1, "Pricey Bear", 4, 4)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    // MV 0 — BELOW the tie, and neither an artifact nor a creature, so it must be
    // outside both the candidate set and the ranked population.
    let land = scenario.add_basic_land(P0, ManaColor::Green);

    let mut runner = scenario.build();

    advance_to_trigger_target_selection(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the upkeep trigger must pause for target selection, got {:?}",
        runner.state().waiting_for
    );

    let legal = legal_target_ids(&runner);
    assert!(
        !legal.is_empty(),
        "reach-guard: ranking over the disjunctive population must leave the MV 2 tie \
         legal; an empty set means the Land's MV 0 set the bar, i.e. the population \
         was the wider [Permanent] set"
    );
    assert!(
        legal.contains(&artifact) && legal.contains(&creature),
        "both MV 2 permanents tie for the population minimum, one per Or leg; legal={legal:?}"
    );
    assert!(
        !legal.contains(&expensive),
        "MV 5 is not the lowest — dropping the aggregate makes this legal again"
    );
    assert!(
        !legal.contains(&land),
        "a Land is neither an artifact nor a creature and must be excluded from the \
         candidates; legal={legal:?}"
    );
}

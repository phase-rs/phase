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
use engine::types::mana::ManaCost;
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

    let mut runner = scenario.build();
    // A Land must be excluded by the `Non(Land)` leg regardless of its mana value.
    let land = runner.state().battlefield.iter().copied().find(|id| {
        runner.state().objects[id]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Land)
    });

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
    if let Some(land) = land {
        assert!(
            !legal.contains(&land),
            "a Land is excluded by the Non(Land) leg of the noun phrase"
        );
    }
}

//! Issue #750 — Riku, of Many Paths: "Whenever you cast a MODAL spell, choose
//! up to X …" must fire ONLY on modal spells, not on every spell its controller
//! casts.
//!
//! ROOT CAUSE: the parser dropped the "modal" qualifier because `FilterProp`
//! could not express modality, leaving the SpellCast trigger's `valid_card` as
//! `None` — which matches every spell (over-trigger). The fix adds
//! `FilterProp::Modal` (CR 700.2) and evaluates it against the stack object's
//! static printed modality (`obj.modal.is_some()`), a printed characteristic
//! populated at object creation and available at SpellCast-trigger match time.
//!
//! These are full cast-pipeline tests: Riku sits on P0's battlefield, P0 casts a
//! spell through `GameRunner::cast(..).commit()`, and we assert whether Riku's
//! SpellCast trigger landed on the stack as a `TriggeredAbility` (CR 603.3 —
//! triggers go on the stack the next time a player would receive priority, which
//! is the post-cast Priority window `commit()` halts at).
//!
//! CARD TEXT is this engine's authoritative Oracle text for each card:
//!   * Abrade — "Choose one —\n• Abrade deals 3 damage to target creature.\n•
//!     Destroy target artifact." — a real MODAL instant (`obj.modal.is_some()`).
//!   * Shock — "Shock deals 2 damage to any target." — a plain NON-MODAL instant.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const RIKU_ORACLE: &str = "Whenever you cast a modal spell, choose up to X, where X is the number of times you chose a mode for that spell —\n\u{2022} Exile the top card of your library. Until the end of your next turn, you may play it.\n\u{2022} Put a +1/+1 counter on Riku. It gains trample until end of turn.\n\u{2022} Create a 1/1 blue Bird creature token with flying.";

const ABRADE_ORACLE: &str =
    "Choose one \u{2014}\n\u{2022} Abrade deals 3 damage to target creature.\n\u{2022} Destroy target artifact.";
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

/// Add `count` red mana to P0's pool so the auto-pay path funds the cast
/// (mirrors the Chord/Green Sun's Zenith harness — deterministic payment
/// without modelling lands).
fn add_red_mana(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Count Riku's SpellCast trigger instances currently on the stack (matched by
/// the trigger's source object id).
fn riku_triggers_on_stack(state: &engine::types::game_state::GameState, riku: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == riku && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

/// Cast `spell` (announcing `modes`/`targets` for the spell's own choices) and
/// drive the pipeline only until the spell has committed to the stack and the
/// post-cast trigger-placement window is reached (CR 603.3): either the
/// `Priority` window, or — when the fired trigger is itself modal — Riku's own
/// `AbilityModeChoice`. At either boundary the trigger, if it fired, is already
/// on the stack. Returns the number of Riku triggers observed on the stack.
///
/// This is a purpose-built driver (not `SpellCast::commit`) because Riku's
/// modal trigger surfaces `AbilityModeChoice` before the `Priority` window,
/// which the general commit driver intentionally rejects.
fn cast_and_count_riku_triggers(
    runner: &mut GameRunner,
    spell: ObjectId,
    riku: ObjectId,
    modes: &[usize],
    targets: &[ObjectId],
) -> usize {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("CastSpell must be accepted");

    let mut remaining_targets = targets.to_vec();
    for _ in 0..64 {
        match &runner.state().waiting_for {
            // Spell's own modal choice (Abrade "Choose one").
            WaitingFor::ModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("SelectModes must be accepted");
            }
            // Spell's own target slot(s).
            WaitingFor::TargetSelection { .. } => {
                let target = remaining_targets.remove(0);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(target)),
                    })
                    .expect("ChooseTarget must be accepted");
            }
            // Trigger-placement boundary: the spell is on the stack and any
            // SpellCast trigger has been placed above it. Riku's modal trigger
            // surfaces its own AbilityModeChoice here; a non-firing cast reaches
            // Priority. Either way, inspect the stack now.
            WaitingFor::Priority { .. } | WaitingFor::AbilityModeChoice { .. } => {
                return riku_triggers_on_stack(runner.state(), riku);
            }
            other => panic!(
                "unexpected WaitingFor while driving the cast: {}",
                other.variant_name()
            ),
        }
    }
    panic!("cast pipeline did not reach a trigger-placement boundary within the step budget");
}

/// RUNTIME POSITIVE: casting a MODAL spell (Abrade) fires Riku's trigger.
#[test]
fn riku_fires_on_modal_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Riku on P0's battlefield watching for modal casts.
    let riku = scenario
        .add_creature_from_oracle(P0, "Riku, of Many Paths", 2, 4, RIKU_ORACLE)
        .id();
    // A target creature for Abrade's "3 damage to target creature" mode.
    let dummy = scenario.add_creature(P1, "Target Dummy", 3, 3).id();
    let abrade = scenario
        .add_spell_to_hand_from_oracle(P0, "Abrade", true, ABRADE_ORACLE)
        .id();
    let mut runner = scenario.build();
    add_red_mana(&mut runner, 2);

    assert!(
        runner.state().objects[&abrade].modal.is_some(),
        "Abrade must be modal (obj.modal.is_some()) — the runtime predicate this test rests on"
    );
    let fired = cast_and_count_riku_triggers(&mut runner, abrade, riku, &[0], &[dummy]);
    assert_eq!(
        fired, 1,
        "Riku's SpellCast trigger MUST fire when a modal spell is cast (CR 700.2)"
    );
}

/// RUNTIME NEGATIVE (load-bearing): casting a PLAIN NON-MODAL spell (Shock) does
/// NOT fire Riku's trigger. Reverting the fix leaves `valid_card == None`, and
/// Shock wrongly fires (the #750 over-trigger).
#[test]
fn riku_does_not_fire_on_non_modal_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let riku = scenario
        .add_creature_from_oracle(P0, "Riku, of Many Paths", 2, 4, RIKU_ORACLE)
        .id();
    // A creature so Shock's "any target" has a legal referent.
    let dummy = scenario.add_creature(P1, "Target Dummy", 3, 3).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let mut runner = scenario.build();
    add_red_mana(&mut runner, 1);

    assert!(
        runner.state().objects[&shock].modal.is_none(),
        "Shock must be non-modal (obj.modal.is_none())"
    );
    // Shock is non-modal and has a single "any target" slot.
    let fired = cast_and_count_riku_triggers(&mut runner, shock, riku, &[], &[dummy]);
    assert_eq!(
        fired, 0,
        "Riku's trigger MUST NOT fire on a non-modal spell — reverting the fix over-triggers here (#750)"
    );
}

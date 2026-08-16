//! The Mind Stone (MSH) — Harness keyword action (CR 701.64) + ∞ (Infinity)
//! ability gate (CR 702.186).
//!
//! Oracle:
//!   Indestructible
//!   {T}: Add {W}.
//!   {5}{W}, {T}: Harness The Mind Stone. (Once harnessed, its ∞ ability is active.)
//!   ∞ — At the beginning of your end step, exile up to one other target nonland
//!       permanent you control, then return that card to the battlefield under
//!       its owner's control.
//!
//! Discriminating coverage (all drive the real engine pipeline):
//!  * `harness_activation_marks_harnessed` — paying {5}{W},{T} through
//!    `activate().resolve()` (the production `ActivateAbility` route) sets the
//!    `harnessed` designation. Revert probe: drop `Effect::Harness`/its resolver
//!    and the effect parses as `Unimplemented` (a no-op) — `harnessed` stays
//!    false and the assertion fails.
//!  * `infinity_trigger_does_not_fire_before_harness` — at the controller's end
//!    step, the ∞ trigger must NOT be placed on the stack while unharnessed.
//!    Revert probe: drop the `TriggerCondition::SourceIsHarnessed` mapping and
//!    the trigger fires unconditionally — it would appear on the stack here.
//!  * `infinity_trigger_fires_after_harness` — once harnessed, the ∞ trigger IS
//!    placed on the stack at the controller's end step. This is the positive arm
//!    that the negative arm above discriminates against.

use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::GameScenario;
use engine::types::ability::ActivationRestriction;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

const MIND_STONE_ORACLE: &str = "Indestructible\n\
{T}: Add {W}.\n\
{5}{W}, {T}: Harness The Mind Stone. (Once harnessed, its \u{221e} ability is active.)\n\
\u{221e} \u{2014} At the beginning of your end step, exile up to one other target \
nonland permanent you control, then return that card to the battlefield under its \
owner's control.";

/// CR 602.1b: the activation driver pays the Harness cost from the pool. Fund
/// {5}{W} as five colorless + one white (auto-tap of mana sources is not
/// modeled by the harness).
fn fund_harness_cost(state: &mut GameState) {
    for _ in 0..5 {
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    state.players[0]
        .mana_pool
        .add(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
}

/// True when an ∞ triggered ability sourced by `stone` is currently on the stack.
fn infinity_trigger_on_stack(state: &GameState, stone: ObjectId) -> bool {
    state.stack.iter().any(|entry| {
        entry.source_id == stone && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    })
}

/// CR 701.64b + CR 702.186b: an ∞ ("∞ — [Ability]") ACTIVATED ability is
/// present (and therefore activatable) only while the source is harnessed. This
/// drives the real parser → `can_activate_ability_now` and the production
/// activation pipeline.
///
/// Revert probe: removing the parser `push` of
/// `ActivationRestriction::SourceIsHarnessed` (Priority 4 in `oracle.rs`) OR the
/// runtime arm in `restrictions.rs` makes assertion (b) fail — the unharnessed ∞
/// ability becomes activatable.
#[test]
fn infinity_activated_ability_gated_by_harness() {
    // Two activated abilities, in source order:
    //   index 0: `{2}, {T}: Harness this artifact.`
    //   index 1: `∞ — {T}: Add {C}.`  (the gated ability)
    const ORACLE: &str = "{2}, {T}: Harness this artifact.\n\u{221e} \u{2014} {T}: Add {C}.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stone = scenario
        .add_creature_from_oracle(P0, "Harness Mana Rock", 0, 1, ORACLE)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    // Empirically confirmed indices (see the matching debug dump in the
    // implementation report): exactly 2 abilities; index 1 is the ∞ ability
    // carrying SourceIsHarnessed, index 0 is the Harness activator.
    const HARNESS_IDX: usize = 0;
    const INFINITY_IDX: usize = 1;

    // (a) Structural non-vacuity guard: the ∞ ability must carry the restriction.
    assert!(
        runner.state().objects[&stone].abilities[INFINITY_IDX]
            .activation_restrictions
            .contains(&ActivationRestriction::SourceIsHarnessed),
        "the ∞ activated ability must carry ActivationRestriction::SourceIsHarnessed; got {:?}",
        runner.state().objects[&stone].abilities[INFINITY_IDX].activation_restrictions
    );

    // (b) Before harness: the ∞ ability is not activatable.
    assert!(
        !runner.state().objects[&stone].harnessed,
        "precondition: the source starts unharnessed"
    );
    assert!(
        !can_activate_ability_now(runner.state(), P0, stone, INFINITY_IDX),
        "CR 702.186b: the ∞ activated ability must NOT be activatable while unharnessed"
    );

    // (c) Harness via the production activation pipeline. Fund the {2} generic.
    for _ in 0..2 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    runner.activate(stone, HARNESS_IDX).resolve();
    assert!(
        runner.state().objects[&stone].harnessed,
        "activating the Harness ability must set the harnessed designation"
    );

    // (d) Isolate the harnessed gate from the {T} tap cost of the ∞ ability
    // (the Harness cost tapped the source).
    runner.state_mut().objects.get_mut(&stone).unwrap().tapped = false;

    // (e) After harness: the ∞ ability becomes activatable.
    assert!(
        can_activate_ability_now(runner.state(), P0, stone, INFINITY_IDX),
        "CR 702.186b: once harnessed, the ∞ activated ability must be activatable"
    );
}

#[test]
fn harness_activation_marks_harnessed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The Mind Stone is an artifact; the Harness/∞ behavior is type-agnostic, so
    // a 0/1 body keeps the scenario minimal (mirrors the Roiling Vortex harness).
    let stone = scenario
        .add_creature_from_oracle(P0, "The Mind Stone", 0, 1, MIND_STONE_ORACLE)
        .id();
    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&stone].harnessed,
        "The Mind Stone starts unharnessed"
    );

    fund_harness_cost(runner.state_mut());

    // Ability index 1 is the {5}{W},{T} Harness ability (index 0 is the mana
    // ability). `resolve()` drives the production ActivateAbility → mana-payment
    // → resolution pipeline.
    runner.activate(stone, 1).resolve();

    assert!(
        runner.state().objects[&stone].harnessed,
        "activating the Harness ability ({{5}}{{W}},{{T}}) must set the harnessed designation"
    );
    assert!(
        runner.state().objects[&stone].tapped,
        "the Harness cost includes {{T}} — the source must be tapped"
    );
}

/// Roll the turn forward from P0's pre-combat main until either the ∞ trigger
/// from `stone` is observed on the stack (or as a pending target selection), or
/// P0's end step has been fully drained without it firing. Returns whether the
/// trigger fired. The loop passes priority and answers combat prompts so it
/// never stalls (mirrors the Roiling Vortex upkeep-trigger harness).
fn run_to_end_step_observing_trigger(
    runner: &mut engine::game::scenario::GameRunner,
    stone: ObjectId,
) -> bool {
    let mut reached_end_step = false;
    for _ in 0..400 {
        if infinity_trigger_on_stack(runner.state(), stone) {
            return true;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            return true;
        }
        if runner.state().phase == Phase::End {
            reached_end_step = true;
        }
        let acted = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            _ => break,
        };
        if acted.is_err() {
            break;
        }
        // Once past P0's end step into cleanup/next turn, stop — the trigger had
        // its window and (for the negative case) did not fire.
        if reached_end_step && runner.state().phase != Phase::End {
            break;
        }
    }
    false
}

#[test]
fn infinity_trigger_does_not_fire_before_harness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stone = scenario
        .add_creature_from_oracle(P0, "The Mind Stone", 0, 1, MIND_STONE_ORACLE)
        .as_artifact()
        .id();
    // A second noncreature permanent the ∞ ability could blink, so the only
    // reason the trigger would not fire is the harnessed gate (not an empty
    // target set). Artifact-typed so combat never stalls the roll-forward.
    scenario.add_creature(P0, "Ornithopter", 0, 2).as_artifact();
    let mut runner = scenario.build();

    assert!(!runner.state().objects[&stone].harnessed);

    // The ∞ trigger's intervening-if (`SourceIsHarnessed`, CR 603.4) must reject
    // placement while unharnessed across P0's entire end step.
    let fired = run_to_end_step_observing_trigger(&mut runner, stone);
    assert!(
        !fired,
        "the ∞ trigger must NOT fire while the source is unharnessed (CR 702.186b)"
    );
}

#[test]
fn infinity_trigger_fires_after_harness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stone = scenario
        .add_creature_from_oracle(P0, "The Mind Stone", 0, 1, MIND_STONE_ORACLE)
        .as_artifact()
        .id();
    scenario.add_creature(P0, "Ornithopter", 0, 2).as_artifact();
    let mut runner = scenario.build();

    // Harness it through the production activation pipeline.
    fund_harness_cost(runner.state_mut());
    runner.activate(stone, 1).resolve();
    assert!(
        runner.state().objects[&stone].harnessed,
        "precondition: The Mind Stone is harnessed"
    );

    let fired = run_to_end_step_observing_trigger(&mut runner, stone);
    assert!(
        fired,
        "the ∞ trigger must fire at the controller's end step once harnessed (CR 702.186b)"
    );
}

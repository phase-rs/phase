//! Issue #9180 — a `ForceBlock` effect naming an attacker (`{G}: Target
//! creature blocks this creature this turn if able.`) discarded its
//! requirement when the named attacker wasn't attacking YET at resolution.
//! `force_block::resolve` split the resolution-time liveness question
//! (CR 400.7, `ObjectIncarnationRef::is_current`) from the declare-blockers
//! applicability question (CR 509.1c, `combat::BlockDeclarationConstraints::build`),
//! which the unit tests in `force_block.rs` pin at the resolver level. These
//! tests drive the real activation/attack/block pipeline (CR 611.2a) to prove
//! the recorded requirement is actually enforced.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExtraPhase, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const TANGLE_ANGLER_ABILITY: &str = "{G}: Target creature blocks this creature this turn if able.";

/// Bucket-B card from the class fix's landfall-trigger side (not
/// attack-gated): "Landfall — Whenever a land you control enters, you may
/// have target creature block this creature this turn if able." Verified
/// against `data/mtgjson/AtomicCards.json` this session; the Deathtouch
/// reminder line is passed separately via `&["Deathtouch"]` for the same
/// keyword-hint reason Tangle Angler's `&["Infect"]` is (this file's shared
/// convention, not a correctness requirement — see
/// `assert_sole_ability_is_force_block`'s doc comment for what was measured).
const TURNTIMBER_BASILISK_ORACLE: &str = "Landfall — Whenever a land you control enters, you may \
                                           have target creature block this creature this turn if \
                                           able.";

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Assert the fixture's `source` carries exactly one ability and that it is
/// the `ForceBlock` activated ability at index 0 — the assumption
/// `activate(source, 0)` below relies on. Measured this session (see the
/// executor report's M7 row): restoring Tangle Angler's Infect reminder line
/// into the Oracle text passed to `from_oracle_text_with_keywords` did NOT
/// shift the ability off index 0 — reminder-text stripping correctly
/// collapsed it to zero spurious abilities in this fixture. The guard is kept
/// anyway: it costs nothing and documents the index assumption explicitly
/// rather than leaving it implicit.
fn assert_sole_ability_is_force_block(runner: &GameRunner, source: ObjectId) {
    let obj = &runner.state().objects[&source];
    // `AbilityDefinition` derives `Clone, PartialEq, Eq` only (no `Debug`), so
    // the failure message prints each ability's `Effect` (which IS `Debug`)
    // rather than the ability list itself.
    let effects: Vec<&Effect> = obj.abilities.iter().map(|a| a.effect.as_ref()).collect();
    assert_eq!(
        obj.abilities.len(),
        1,
        "reach guard: the fixture must carry exactly one ability, got {effects:?}"
    );
    assert!(
        matches!(*obj.abilities[0].effect, Effect::ForceBlock { .. }),
        "reach guard: ability 0 must be the ForceBlock ability, got {:?}",
        obj.abilities[0].effect.as_ref()
    );
}

/// Declare `attackers` (all against P1) and drive to `WaitingFor::DeclareBlockers`,
/// stopping BEFORE submitting any blocks. Mirrors the declare-attackers half of
/// `rules::run_combat_with_blocker_divisions` (`crates/engine/tests/integration/rules.rs`),
/// which this file cannot reuse directly: that helper `.expect()`s the
/// `DeclareBlockers` submission internally, but I1–I3 below need to observe a
/// REJECTED empty declaration without panicking.
fn attack_and_reach_declare_blockers(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.pass_both_players();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("DeclareAttackers should succeed");

    // CR 603.3b: same-controller attack triggers may surface an ordering prompt.
    while matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        let n = if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
            triggers.len()
        } else {
            0
        };
        runner
            .act(GameAction::OrderTriggers {
                order: (0..n).collect(),
            })
            .expect("OrderTriggers should succeed");
    }
    if !runner.state().stack.is_empty() {
        runner.advance_until_stack_empty();
    }
    // CR 508.2: Active player gets priority after attackers — pass through it.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
}

/// Drive a landfall trigger's `WaitingFor::TriggerTargetSelection` (preferring
/// `want`) and `WaitingFor::OptionalEffectChoice` (answered with `accept`)
/// windows to a clean empty-stack Priority. Mirrors
/// `integration_landfall.rs::resolve_landfall_batch`'s two-window handling.
/// Returns whether a target-selection window was ever observed, so I3 can
/// reach-guard that the trigger actually went on the stack (CR 603.3d) rather
/// than silently never firing.
fn drain_landfall_trigger(runner: &mut GameRunner, want: ObjectId, accept: bool) -> bool {
    let mut saw_target_prompt = false;
    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return saw_target_prompt;
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                saw_target_prompt = true;
                let slot = &target_slots[selection.current_slot];
                let pick = slot
                    .legal_targets
                    .iter()
                    .find(|t| matches!(t, TargetRef::Object(id) if *id == want))
                    .or_else(|| slot.legal_targets.first())
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: pick })
                    .expect("choose a legal target for the landfall trigger");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("decide the landfall trigger's optional");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return saw_target_prompt;
                }
            }
        }
    }
    saw_target_prompt
}

/// CR 611.2a + CR 509.1c (issue #9180, Bucket B — activated-ability half):
/// activating Tangle Angler's ability in precombat main, before Angler has
/// ever attacked, must still force Bear to block once Angler attacks.
#[test]
fn tangle_angler_forces_a_block_before_it_attacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let angler = scenario
        .add_creature(P0, "Tangle Angler", 1, 5)
        .from_oracle_text_with_keywords(&["Infect"], TANGLE_ANGLER_ABILITY)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    assert_sole_ability_is_force_block(&runner, angler);

    let before = runner.state().clone();
    add_mana(&mut runner, ManaType::Green, 1);
    let outcome = runner.activate(angler, 0).target_object(bear).resolve();
    // I1-RG: the cost was actually paid and the ability actually resolved off
    // the stack, not merely accepted onto it.
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "reach guard: the {{G}} cost must have drained the mana pool"
    );
    assert_eq!(
        outcome.stack_size(),
        0,
        "reach guard: the ability must have resolved off the stack"
    );

    // The issue's actual user-visible symptom: the player reported seeing
    // nothing in the game log for this resolution. `resolve_log_entries`
    // (`engine::game::log`) is the single production authority that turns
    // engine events into the log the frontend renders — `EffectResolved`
    // (`log.rs`'s `GameEvent::EffectResolved` arm) renders as `"<source>'s
    // effect resolves"` and is not in `should_exclude_event`'s drop set, so
    // this is a coverage gap, not a defect (confirmed by running it here,
    // not by reading the source alone).
    let entries =
        engine::game::log::resolve_log_entries(outcome.events(), &before, outcome.state());
    // Checked against the EffectResolved entry's OWN segments, not a
    // flattened-log substring search: measured this round that
    // `rendered.contains("Tangle Angler") && rendered.contains("effect
    // resolves")` stayed green even after replacing `card_seg(state,
    // *source_id)` in `log.rs` with a literal (rendering `"Nobody's effect
    // resolves"`), because the unrelated `activates ability` entry still
    // supplies "Tangle Angler" on its own. Matching this entry's segments
    // directly against `LogSegment::CardName { object_id: angler, .. }`
    // followed by `LogSegment::Text("'s effect resolves")` ties the
    // assertion to the EffectResolved rendering specifically, and reddens
    // under that exact mutation (confirmed this round, then reverted).
    let effect_resolved_entry = entries.iter().find(|entry| {
        matches!(
            entry.segments.as_slice(),
            [
                engine::types::log::LogSegment::CardName { object_id, .. },
                engine::types::log::LogSegment::Text(suffix)
            ] if *object_id == angler && suffix == "'s effect resolves"
        )
    });
    assert!(
        effect_resolved_entry.is_some(),
        "the ForceBlock ability's resolution must produce an EffectResolved \
         log entry naming Angler as its own source, got entries: {entries:?}"
    );

    attack_and_reach_declare_blockers(&mut runner, &[angler]);

    assert!(
        runner.declare_blockers(&[]).is_err(),
        "CR 509.1c: Bear must be required to block Angler even though the \
         requirement was recorded before this combat existed"
    );
    // Paired positive (measured requirement): proves the engine really is at
    // DeclareBlockers waiting for a submission, not merely erroring at some
    // other prompt — `declare_blockers(&[])` returns `Err` for either reason.
    assert!(
        runner.declare_blockers(&[(bear, angler)]).is_ok(),
        "reach guard: declaring the required block must be legal"
    );
}

/// CR 509.1c's own named case: "If a requirement ... refers to a turn with
/// multiple combat phases, the creature blocks if able during EACH declare
/// blockers step in that turn." The `UntilEndOfTurn` requirement recorded in
/// combat 1 must still be enforced at combat 2's declare-blockers step.
#[test]
fn tangle_angler_requirement_persists_into_a_second_combat_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Vigilance so Angler is still untapped for the second attack declaration
    // (I2 blocker, measured by review: a creature that attacked in combat 1 is
    // tapped at declaration (CR 508.1f) and rejected by combat 2's
    // `validate_attackers` with no untap step of its own). Applied AFTER
    // `from_oracle_text_with_keywords` — that call overwrites builder-set
    // keywords, so `.vigilance()` must come last in the chain.
    let angler = scenario
        .add_creature(P0, "Tangle Angler", 1, 5)
        .from_oracle_text_with_keywords(&["Infect"], TANGLE_ANGLER_ABILITY)
        .vigilance()
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    assert_sole_ability_is_force_block(&runner, angler);

    add_mana(&mut runner, ManaType::Green, 1);
    runner.activate(angler, 0).target_object(bear).resolve();

    // Combat 1: submit the required block so combat actually completes.
    attack_and_reach_declare_blockers(&mut runner, &[angler]);
    runner
        .declare_blockers(&[(bear, angler)])
        .expect("combat 1's required block must be legal");

    // Drive combat 1 through the combat-damage step and INTO the EndCombat
    // step's priority window — not just past declare-blockers.
    // `turns.rs::advance_phase_once` only calls `complete_end_combat_teardown`
    // (the sole production site that prunes combat-scoped transient continuous
    // effects, CR 511.3) when LEAVING `Phase::EndCombat`. Measured this
    // session: capturing the `ExtraPhase` anchor immediately after
    // `declare_blockers` (as this test did before this round's fix) captures
    // `Phase::DeclareBlockers`, and the phase trace between the two combats
    // read `[DeclareBlockers, BeginCombat, DeclareAttackers]` —
    // `Phase::EndCombat` was never in it, so the teardown never ran.
    for _ in 0..40 {
        if runner.state().phase == Phase::EndCombat
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            let n = if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for
            {
                triggers.len()
            } else {
                0
            };
            runner
                .act(GameAction::OrderTriggers {
                    order: (0..n).collect(),
                })
                .expect("OrderTriggers should succeed");
        } else if !runner.state().stack.is_empty() {
            runner.advance_until_stack_empty();
        } else if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    // Reach guard, proven load-bearing this round: replacing this loop's
    // `0..40` with `0..0` (so combat 1's drive never runs) reddens this
    // assertion directly (`left: DeclareBlockers, right: EndCombat`).
    assert_eq!(
        runner.state().phase,
        Phase::EndCombat,
        "combat 1 must reach the EndCombat step's priority window before the \
         second combat is scheduled"
    );

    // CR 500.8: schedule an extra combat phase anchored to `Phase::EndCombat`.
    // `ureni_attack_trigger.rs::ureni_attacks_in_second_combat_fires_again`
    // documents the same anchor choice: "the trigger resolver pushes with
    // anchor = EndCombat and the engine then advances out of EndCombat into
    // the extra BeginCombat" (CR 500.8: an extra phase is inserted directly
    // after its anchor phase). This `anchor: Phase::EndCombat` literal is the
    // line this test actually discriminates: proven this round by changing
    // it to `Phase::DeclareBlockers`, which reddens the `DeclareAttackers`
    // reach guard below (`left: Draw, right: DeclareAttackers` — the extra
    // phase is never inserted, so the drive below runs out its 60 iterations
    // into the next turn instead).
    runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: Phase::EndCombat,
        phase: Phase::BeginCombat,
        attacker_restriction: None,
        attacker_restriction_source: None,
    });

    for _ in 0..60 {
        if runner.state().phase == Phase::DeclareAttackers
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    // Reach guard: the second combat's declare-attackers step must actually
    // have been reached — otherwise everything below is vacuous.
    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "extra combat phase must have reached DeclareAttackers"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "engine must be waiting for attacker declaration in the extra combat"
    );

    runner
        .declare_attackers(&[(angler, AttackTarget::Player(P1))])
        .expect("Angler must be able to attack again (vigilance keeps it untapped)");

    while matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        let n = if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
            triggers.len()
        } else {
            0
        };
        runner
            .act(GameAction::OrderTriggers {
                order: (0..n).collect(),
            })
            .expect("OrderTriggers should succeed");
    }
    if !runner.state().stack.is_empty() {
        runner.advance_until_stack_empty();
    }
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }

    assert!(
        runner.declare_blockers(&[]).is_err(),
        "CR 509.1c: the UntilEndOfTurn requirement must still be enforced at \
         the SECOND declare-blockers step of the same turn"
    );
    // Paired positive (measured requirement): proves the second declaration
    // really did reach DeclareBlockers rather than erroring for some other
    // reason (e.g. a stale prompt from combat 1).
    assert!(
        runner.declare_blockers(&[(bear, angler)]).is_ok(),
        "reach guard: declaring the required block must still be legal in combat 2"
    );
}

/// Class-axis pin: the fix is "not attack-gated", not "activated vs
/// triggered". Turntimber Basilisk's landfall trigger (`ChangesZone`, not
/// `Attacks`) exercises the trigger half of Bucket B — it names Turntimber
/// Basilisk itself as the attacker via a fresh landfall event, well before
/// any combat exists, exactly like Tangle Angler's activated ability.
#[test]
fn turntimber_basilisk_landfall_records_an_enforced_requirement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let basilisk = scenario
        .add_creature(P0, "Turntimber Basilisk", 4, 4)
        .from_oracle_text_with_keywords(&["Deathtouch"], TURNTIMBER_BASILISK_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    // NOTE: `assert_sole_ability_is_force_block` is Tangle Angler's ACTIVATED-
    // ability guard (`GameObject::abilities`) and does not apply here — a
    // landfall TRIGGER lives in `GameObject::triggers`, not `abilities`, and
    // there is no `activate(source, index)` call in this test for an index
    // shift to corrupt. `saw` below is this test's reach guard instead: it
    // pins that the trigger actually fired, which is the failure mode this
    // row exists to catch.
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("should play Forest");

    let saw = drain_landfall_trigger(&mut runner, bear, true);
    assert!(
        saw,
        "reach guard: landfall must have gone on the stack and asked for a target \
         before the requirement can be considered recorded"
    );

    attack_and_reach_declare_blockers(&mut runner, &[basilisk]);

    assert!(
        runner.declare_blockers(&[]).is_err(),
        "CR 509.1c: Bear must be required to block Basilisk — the trigger fired \
         and recorded the requirement before any combat existed"
    );
    assert!(
        runner.declare_blockers(&[(bear, basilisk)]).is_ok(),
        "reach guard: declaring the required block must be legal"
    );
}

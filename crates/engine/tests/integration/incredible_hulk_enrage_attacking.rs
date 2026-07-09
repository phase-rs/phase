//! The Incredible Hulk (MSH) — Enrage's "If he's attacking," rider must GATE
//! the additional-combat-phase (and untap) sub-effects while leaving the +1/+1
//! counter ungated.
//!
//! Oracle: "Reach, trample\nEnrage — Whenever The Incredible Hulk is dealt
//! damage, put a +1/+1 counter on him. If he's attacking, untap him and there
//! is an additional combat phase after this phase."
//!
//! The recognizer already yields `StaticCondition::SourceIsAttacking` for "he's
//! attacking"; the fix bridges it to `AbilityCondition::SourceMatchesFilter {
//! Typed([Attacking]) }` in `static_condition_to_ability_condition` so the
//! parsed sub-ability carries the gate. CR 608.2c: instructions resolve in
//! written order and the rider modifies the later sub-effects only. CR 207.2c:
//! Enrage is an ability word (no rules meaning) — the line is a plain
//! `DamageReceived` trigger.
//!
//! Discriminating observable: the **additional combat phase** (`state.extra_phases`)
//! is scheduled iff Hulk is attacking. The ONLY variable between the two tests is
//! whether Hulk is in `state.combat.attackers`; the damage source is held constant
//! (direct `Effect::DealDamage` to Hulk). Reverting the bridge (the arm back to
//! `=> None`) drops the condition, so the gated chain fires unconditionally and
//! Test B's `extra_phases.is_empty()` assertion flips to red.
//!
//! The chained "untap him" (`SetTapState { target: ParentTarget }`) was a
//! pre-existing no-op for this card — the head `PutCounter` targets `SelfRef`,
//! which is not materialized into `ability.targets` (issue #323), so the chained
//! `ParentTarget` inherited no object to untap. That targeting-path gap (distinct
//! from this condition-bridge fix) is now closed at parse time:
//! `sequence::patch_self_ref_head_tap_anaphor` rewrites the chained
//! single-permanent `SetTapState`'s `ParentTarget` to `SelfRef` whenever the
//! antecedent head's subject is `SelfRef`, so the untap binds the source via
//! `tap_untap_target_ids`' `SelfRef` arm. A head with a real or optional target
//! (Tyvar Kell "...up to one target Elf. Untap it.") keeps `ParentTarget`, so a
//! declined optional target correctly no-ops (CR 608.2b — the anaphor has no
//! referent). `enrage_untaps_hulk_when_attacking` (Test C) exercises Hulk's untap
//! directly; the condition-bridge tests (A/B) keep their independent observable on
//! the additional-combat-phase (`state.extra_phases`), which the gate *does*
//! control, so the gate discriminator stays revert-proven.

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{
    AbilityCondition, Effect, FilterProp, QuantityExpr, ResolvedAbility, SubAbilityLink,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::ExtraPhase;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const HULK_ORACLE: &str = "Reach, trample\n\
Enrage — Whenever The Incredible Hulk is dealt damage, put a +1/+1 counter on \
him. If he's attacking, untap him and there is an additional combat phase after \
this phase.";

/// A no-cost direct-damage source (an opponent's effect) used to fire Enrage.
fn damage_ability(source: ObjectId, target: ObjectId, amount: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Object(target)],
        source,
        P1,
    )
}

struct EnrageOutcome {
    plus_counters: u32,
    extra_phase_scheduled: bool,
    hulk_tapped: bool,
}

/// Build Hulk on the battlefield, deal it direct damage to fire Enrage, resolve
/// the trigger through the real trigger→stack→resolution pipeline, and report
/// the observables. When `attacking` is true, Hulk is placed in
/// `state.combat.attackers` (CR 508.1k) — the only variable across the two cases.
fn run_enrage(attacking: bool) -> EnrageOutcome {
    let mut scenario = GameScenario::new();
    let hulk = scenario
        .add_creature_from_oracle(P0, "The Incredible Hulk", 8, 8, HULK_ORACLE)
        .id();
    // An opponent permanent to act as the damage source.
    let source = scenario.add_creature(P1, "Damage Source", 2, 2).id();
    let mut runner = scenario.build();

    // CR 500.10a: the additional-combat-phase guard only adds the phase to the
    // controller's own turn — make Hulk's controller the active player.
    runner.state_mut().active_player = P0;

    // Pre-tap Hulk so the chained "untap him" rider has an observable to flip.
    // CR 508.1f: a declared attacker is normally tapped; for the not-attacking
    // control a tapped Hulk is simply a tapped permanent — attacking-status
    // remains the sole variable across the two cases.
    runner.state_mut().objects.get_mut(&hulk).unwrap().tapped = true;

    if attacking {
        // CR 508.1k + CR 506.4: a declared attacker remains an attacking
        // creature (being dealt damage is not a removal-from-combat condition),
        // so the gate reads it as attacking when the trigger resolves. The gate
        // (`FilterProp::Attacking`) is a LIVE read of `state.combat.attackers`,
        // so injecting the combat membership directly isolates attacking-status
        // as the sole variable.
        runner.state_mut().combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: hulk,
                defending_player: P1,
                attack_target: AttackTarget::Player(P1),
                blocked: false,
                band_id: None,
            }],
            ..Default::default()
        });
    }

    // Fire Enrage by dealing direct damage to Hulk, then resolve the trigger
    // through the real trigger→stack→resolution pipeline.
    let mut events = Vec::new();
    deal_damage::resolve(
        runner.state_mut(),
        &damage_ability(source, hulk, 2),
        &mut events,
    )
    .expect("damage to Hulk resolves");
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    let state = runner.state();
    EnrageOutcome {
        plus_counters: state.objects[&hulk]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        extra_phase_scheduled: state.extra_phases.contains(&ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
        }),
        hulk_tapped: state.objects[&hulk].tapped,
    }
}

/// Test A — Hulk IS attacking: the gated path FIRES (non-vacuous positive).
/// CR 122.1 counter + CR 500.8 extra combat phase.
#[test]
fn enrage_when_attacking_schedules_extra_combat() {
    let out = run_enrage(true);
    assert_eq!(
        out.plus_counters, 1,
        "ungated +1/+1 counter must always be placed (CR 122.1)"
    );
    assert!(
        out.extra_phase_scheduled,
        "gated additional combat phase must be scheduled while attacking (CR 500.8)"
    );
}

/// Test B — Hulk is NOT attacking: the gate BLOCKS the rider (revert probe).
/// The counter still applies (ungated) but the extra combat phase does not.
#[test]
fn enrage_when_not_attacking_gates_extra_combat() {
    let out = run_enrage(false);
    // Non-vacuity: B is not "nothing happens" — the counter MUST still fire,
    // proving only the rider is gated, not the whole chain.
    assert_eq!(
        out.plus_counters, 1,
        "ungated +1/+1 counter must still be placed when not attacking (CR 122.1)"
    );
    assert!(
        !out.extra_phase_scheduled,
        "no additional combat phase may be scheduled when not attacking (CR 608.2c)"
    );
}

/// Test C — the targeting fix: the chained "untap him" must actually untap Hulk
/// when the attacking gate passes. CR 608.2c + CR 701.26b.
///
/// The head `PutCounter { SelfRef }` never materializes the `SelfRef` antecedent
/// into `ability.targets` (issue #323). `sequence::patch_self_ref_head_tap_anaphor`
/// rewrites the chained `SetTapState`'s `ParentTarget` to `SelfRef` at parse time
/// (the head's subject is `SelfRef`), so at resolution it binds Hulk via
/// `tap_untap_target_ids`' `SelfRef` arm.
///
/// Discrimination: reverting the parse-time patch leaves the sub as
/// `SetTapState { ParentTarget }` with an EMPTY `ability.targets` slot, which the
/// `_` arm reads as `[]` → the untap loop iterates nothing → Hulk stays tapped →
/// the `!hulk_tapped` assertion flips red. The not-attacking control stays green
/// either way (its untap rider is gated off by `SourceMatchesFilter {Attacking}`,
/// so it never reaches the resolver), proving the gate — not the resolver —
/// controls firing.
#[test]
fn enrage_untaps_hulk_when_attacking() {
    let attacking = run_enrage(true);
    assert_eq!(
        attacking.plus_counters, 1,
        "ungated +1/+1 counter must always be placed (CR 122.1)"
    );
    assert!(
        !attacking.hulk_tapped,
        "attacking Hulk must be untapped by the chained 'untap him' (the targeting fix)"
    );

    // Control: not attacking → the untap rider is gated off, so Hulk stays
    // tapped while the ungated counter still applies.
    let not_attacking = run_enrage(false);
    assert_eq!(
        not_attacking.plus_counters, 1,
        "ungated +1/+1 counter must still be placed when not attacking (CR 122.1)"
    );
    assert!(
        not_attacking.hulk_tapped,
        "non-attacking Hulk stays tapped: the untap rider is gated off (CR 608.2c)"
    );
}

/// Building-block-level chain shape (the fastest revert detector): the parsed
/// Enrage trigger must lower to an ungated `PutCounter`, then a `SetTapState`
/// untap gated by `SourceMatchesFilter { Typed([Attacking]) }`, then an
/// `AdditionalPhase` reached as that untap's `ContinuationStep` child (so a
/// false gate transitively skips it — effects/mod.rs resolves only
/// SequentialSibling children of a failed gate).
#[test]
fn enrage_chain_gates_untap_and_extra_combat_on_attacking() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        HULK_ORACLE,
        "The Incredible Hulk",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Hero".to_string()],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "no swallowed-clause / unimplemented warnings expected, got {:?}",
        parsed.parse_warnings
    );
    let trigger = parsed
        .triggers
        .into_iter()
        .next()
        .expect("Enrage damage trigger");
    let head = trigger.execute.expect("trigger execute chain");

    // Head: ungated +1/+1 counter.
    assert!(
        matches!(&*head.effect, Effect::PutCounter { .. }),
        "head must be PutCounter, got {:?}",
        head.effect
    );
    assert_eq!(head.condition, None, "the counter must be ungated");

    // Sub 1: the untap, gated by SourceMatchesFilter{Attacking}.
    let untap = head.sub_ability.as_deref().expect("untap sub-ability");
    // The chained "untap him" must be rewritten from `ParentTarget` to `SelfRef`
    // by `patch_self_ref_head_tap_anaphor` (the SelfRef-subject head is The
    // Incredible Hulk's `PutCounter{SelfRef}`), so at resolution it binds Hulk.
    // Discrimination: reverting the patch leaves this `ParentTarget`.
    assert!(
        matches!(
            &*untap.effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "first sub must be the untap rewritten to SelfRef, got {:?}",
        untap.effect
    );
    assert_eq!(
        untap.condition,
        Some(AbilityCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::Attacking { defender: None }],
                ..Default::default()
            }),
        }),
        "untap must be gated by SourceMatchesFilter{{Attacking}} (the bridge fix)"
    );

    // Sub 2: the additional combat phase, a ContinuationStep child of the gated
    // untap (so a false gate transitively skips it).
    let extra = untap
        .sub_ability
        .as_deref()
        .expect("additional-phase sub-ability");
    assert!(
        matches!(&*extra.effect, Effect::AdditionalPhase { .. }),
        "second sub must be AdditionalPhase, got {:?}",
        extra.effect
    );
    assert_eq!(
        extra.sub_link,
        SubAbilityLink::ContinuationStep,
        "AdditionalPhase must be a ContinuationStep child of the gated untap"
    );
}

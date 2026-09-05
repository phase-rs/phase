//! Phase 2 of the Perplexing Chimera run — "context-ref slot hygiene" (U4, U6:
//! the four `matches!(filter, TargetFilter::SelfRef)` skip predicates in
//! `ability_utils.rs` generalized to `filter.is_context_ref()`) plus the
//! `filter_inner_for_object` `TriggeringSource` arm split (P2.6).
//!
//! Covers Verification Matrix rows V12, V14, V15, V16 (plan-r6
//! §Verification Matrix, Stage-2 rows). V13 (slot/spec/resolver three-way
//! agreement) is an in-crate `#[cfg(test)]` unit in `ability_utils.rs`
//! mirroring the existing `ManaTargetRole` case. V17/V18 (exchange control of
//! a spell, end to end) live in `exchange_control_of_a_spell.rs`.
//!
//! V12 and its sibling drive the real cast pipeline (`GameScenario` /
//! `GameRunner`), per `/card-test`, because the claim under test — whether
//! the trigger reaches the stack at all — depends on the full announcement
//! preflight. V14/V15/V16 call the production resolvers
//! (`exchange_control::resolve`, `create_damage_replacement::resolve`,
//! `change_targets::resolve`) directly against hand-built state, mirroring
//! Phase 1's `spell_controller_is_derived.rs` style: the claims under test are
//! resolver-seam claims, not cast-authorization or target-selection-UI claims.

use engine::game::ability_utils::build_target_slots;
use engine::game::effects::{change_targets, create_damage_replacement, exchange_control};
use engine::game::scenario::{CastCommit, GameScenario, P0, P1};
use engine::game::stack::stack_object_controller;
use engine::game::zones::create_object;
use engine::types::ability::{
    DamageRedirectTarget, Effect, PreventionAmount, RedirectionLifetime, ResolvedAbility,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastingVariant, GameState, RetargetScope, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PERPLEXING_CHIMERA_TEXT: &str = "Whenever an opponent casts a spell, you may exchange \
    control of this creature and that spell. If you do, you may choose new targets for the \
    spell. (If the spell becomes a permanent, you control that permanent.)";

// ---------------------------------------------------------------------------
// V12 — the Chimera trigger reaches the stack
// ---------------------------------------------------------------------------

/// Pass priority until the committed cast's Chimera trigger raises its own
/// `OptionalEffectChoice`, or panic with a diagnosable message if the stack
/// empties first (the pre-fix behavior: the trigger was dropped at
/// announcement preflight, so the opponent's spell alone resolves and the
/// stack goes empty with `waiting_for == Priority`).
fn advance_to_optional_choice(commit: &mut CastCommit<'_>) {
    for _ in 0..40 {
        match commit.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                if commit.state().stack.is_empty() {
                    panic!(
                        "the stack emptied without ever raising an OptionalEffectChoice — a \
                         trigger was dropped or never queued (the pre-fix Perplexing Chimera \
                         bug: no_legal_target_slots() at announcement preflight)"
                    );
                }
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining to the prompt");
            }
            ref other => panic!("unexpected waiting state while draining to the prompt: {other:?}"),
        }
    }
    panic!("did not reach OptionalEffectChoice within 40 iterations");
}

/// V12: P0 has Perplexing Chimera (verbatim Oracle text); P1 casts a real
/// targeted instant (Doom Blade). Assert a `TriggeredAbility` entry sourced
/// from the Chimera reaches the stack, and that advancing to its own
/// resolution lands on ITS `OptionalEffectChoice` — not an empty stack with
/// `waiting_for == Priority` (the pre-fix drop).
///
/// REVERT-FAILING: before P2.1(a), `collect_target_slots_inner` /
/// `build_target_slot_specs`'s `ExchangeControl` arms used
/// `matches!(filter, TargetFilter::SelfRef)`, which does not match
/// `TriggeringSource` — so the generic path surfaced a target slot for it,
/// `legal_targets_for_ability_filter` found no legal targets (a
/// `TriggeringSource` filter is never enumerable — CR 608.2k, `filter.rs`'s
/// `TriggeringSource` arm returns `false`), and `no_legal_target_slots()`
/// dropped the trigger at announcement before it ever reached the stack.
#[test]
fn perplexing_chimera_trigger_reaches_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let chimera = scenario
        .add_creature_from_oracle(P0, "Perplexing Chimera", 3, 3, PERPLEXING_CHIMERA_TEXT)
        .id();
    let victim_creature = scenario.add_creature(P0, "Victim Creature", 2, 2).id();
    let doom_blade = scenario
        .add_spell_to_hand_from_oracle(P1, "Doom Blade", true, "Destroy target nonblack creature.")
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut commit = runner
        .cast(doom_blade)
        .target_object(victim_creature)
        .commit();

    // REACH GUARD: Doom Blade itself must have reached the stack — a rejected
    // cast cannot pass the row vacuously.
    assert!(
        commit.state().stack.iter().any(|e| e.id == doom_blade),
        "Doom Blade must be on the stack after commit"
    );

    // PRIMARY CLAIM: the Chimera's trigger must ALSO be on the stack, sourced
    // from the Chimera object — not dropped at announcement.
    assert!(
        commit.state().stack.iter().any(|e| e.source_id == chimera),
        "Perplexing Chimera's SpellCast trigger must reach the stack (found stack: {:?})",
        commit.state().stack
    );
    assert_eq!(
        commit.state().stack.len(),
        2,
        "exactly two entries: the spell and the trigger"
    );

    // Advance to the trigger's own optional prompt, proving `final_waiting_for`
    // is that prompt (not a drained, empty stack).
    advance_to_optional_choice(&mut commit);
    match commit.state().waiting_for {
        WaitingFor::OptionalEffectChoice { source_id, .. } => {
            assert_eq!(
                source_id, chimera,
                "the optional prompt reached must be the Chimera's own trigger, not some other ability"
            );
        }
        ref other => {
            panic!("expected OptionalEffectChoice sourced from the Chimera, got {other:?}")
        }
    }

    // Accept the exchange and assert it actually swapped control — the
    // parse (zero Effect::Unimplemented) alone cannot pass this row; the
    // trigger must actually DO something once it resolves.
    commit
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the exchange should succeed");
    let spell_entry = commit
        .state()
        .stack
        .iter()
        .find(|e| e.id == doom_blade)
        .expect("Doom Blade remains on the stack")
        .clone();
    assert_eq!(
        stack_object_controller(commit.state(), &spell_entry),
        P0,
        "the exchange must swap the spell to the Chimera's controller (P0)"
    );

    // The chained "you may choose new targets" sub-ability is itself
    // optional; decline it here so this row stays focused on "the trigger
    // reached the stack and did something" — the retarget subject binding is
    // V16's own claim.
    if let WaitingFor::OptionalEffectChoice { .. } = commit.state().waiting_for {
        commit
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("declining the retarget offer should succeed");
    }
}

/// SIBLING (V12): a two-declared-target `ExchangeControl` (Switcheroo) still
/// produces exactly two `TargetSelection` rounds and swaps two battlefield
/// permanents — the P2.1(a) generalization from `SelfRef` to
/// `is_context_ref()` is a strict superset that must not regress the
/// ordinary declared-target path.
#[test]
fn switcheroo_still_surfaces_two_slots_and_swaps_two_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_a = scenario.add_creature(P0, "Creature A", 2, 2).id();
    let creature_b = scenario.add_creature(P1, "Creature B", 3, 3).id();
    let switcheroo = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Switcheroo",
            false,
            "Exchange control of two target creatures.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(switcheroo)
        .target_objects(&[creature_a, creature_b])
        .resolve();

    assert_eq!(
        outcome.state().objects.get(&creature_a).unwrap().controller,
        P1,
        "creature A must swap to P1"
    );
    assert_eq!(
        outcome.state().objects.get(&creature_b).unwrap().controller,
        P0,
        "creature B must swap to P0"
    );
}

/// HOSTILE (V12): `TargetFilter::None` is in `is_context_ref()`'s superset
/// admission — no parser path produces it in an `ExchangeControl` slot, but
/// the widening must not panic.
///
/// WHICH BINDING ACTUALLY HAPPENS: with a DECLARED sibling slot, `None` does
/// NOT reach `resolved_targets`' `use_self` fallback — that fallback is gated
/// on `ability.targets.is_empty()`. It falls through every tier to the
/// terminal `ability.targets.clone()` and binds THE SIBLING'S declared target,
/// so both slots resolve to the same object and CR 701.12b no-ops. This row
/// pins that (the object identity, not just "nothing happened"); the
/// `..._binds_the_source_when_no_sibling_target_exists` row below pins the
/// `use_self` path it is often mistaken for. Together they are the reason
/// `ability_utils.rs`'s slot-builder comment says a NEW context-ref filter
/// must be given a tier in `resolved_targets` before it appears in a parse.
#[test]
fn exchange_control_none_filter_with_a_declared_sibling_binds_that_sibling() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        P0,
        "Source".to_string(),
        Zone::Battlefield,
    );
    let same_controller_target = create_object(
        &mut state,
        CardId(2),
        P0,
        "Same-Controller Target".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&same_controller_target)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    let ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::None,
            target_b: TargetFilter::Typed(TypedFilter::creature()),
        },
        vec![TargetRef::Object(same_controller_target)],
        source,
        P0,
    );
    // REACH GUARD on the actual mechanism: `None` must bind the DECLARED
    // sibling target, not `ability.source_id`. Without this the row would pass
    // under either binding and prove nothing about which one happened.
    assert_eq!(
        engine::game::targeting::resolved_targets(&ability, &TargetFilter::None, &state),
        vec![TargetRef::Object(same_controller_target)],
        "with a declared sibling, None falls through to the declared-targets tier"
    );

    let mut events = Vec::new();
    exchange_control::resolve(&mut state, &ability, &mut events).expect("must not panic");
    assert!(
        state.transient_continuous_effects.is_empty(),
        "CR 701.12b: both slots resolved to the same object ⇒ same-controller no-op"
    );
}

/// SIBLING: the `use_self` path the row above is commonly mistaken for.
/// `resolved_targets` binds `TargetFilter::None` to `ability.source_id` ONLY
/// when `ability.targets` is empty (CR 608.2c) — which, in an
/// `ExchangeControl`, means only when the other slot is also a context ref.
#[test]
fn exchange_control_none_filter_binds_the_source_when_no_sibling_target_exists() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        P0,
        "Source".to_string(),
        Zone::Battlefield,
    );
    let ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::None,
            target_b: TargetFilter::SelfRef,
        },
        vec![],
        source,
        P0,
    );
    assert_eq!(
        engine::game::targeting::resolved_targets(&ability, &TargetFilter::None, &state),
        vec![TargetRef::Object(source)],
        "with no declared targets, None reaches the use_self fallback"
    );
    let mut events = Vec::new();
    exchange_control::resolve(&mut state, &ability, &mut events).expect("must not panic");
    assert!(
        state.transient_continuous_effects.is_empty(),
        "CR 701.12b: both slots are the source ⇒ same-controller no-op"
    );
}

// ---------------------------------------------------------------------------
// V14 — SelfRef still binds through the new context-ref authority
// ---------------------------------------------------------------------------

/// V14: the Avarice Totem / Eyes Everywhere / Phyrexian Infiltrator class —
/// `ExchangeControl{SelfRef, Typed}` still binds `SelfRef` to the ability's
/// own source through `targeting::resolved_targets`' tier-1 short-circuit.
///
/// REVERT-FAILING: routing `SelfRef` through `resolve_event_context_target`
/// instead (the latent regression plan-r1 contained) would return `None` —
/// that resolver has no `SelfRef` arm — producing a TOTAL no-op instead of a
/// swap.
#[test]
fn avarice_totem_class_selfref_still_binds() {
    let mut state = GameState::new_two_player(42);
    let totem = create_object(
        &mut state,
        CardId(1),
        P0,
        "Avarice Totem".to_string(),
        Zone::Battlefield,
    );
    let target = create_object(
        &mut state,
        CardId(2),
        P1,
        "Target Permanent".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::SelfRef,
            target_b: TargetFilter::Typed(TypedFilter::default()),
        },
        vec![TargetRef::Object(target)],
        totem,
        P0,
    );
    let mut events = Vec::new();
    exchange_control::resolve(&mut state, &ability, &mut events).unwrap();

    assert!(
        state.transient_continuous_effects.iter().any(|e| {
            e.affected == TargetFilter::SpecificObject { id: totem } && e.controller == P1
        }),
        "SelfRef (the Totem) must swap to P1"
    );
    assert!(
        state.transient_continuous_effects.iter().any(|e| {
            e.affected == TargetFilter::SpecificObject { id: target } && e.controller == P0
        }),
        "the declared target must swap to P0"
    );
}

/// HOSTILE (V14): the SelfRef source no longer exists (destroyed in
/// response) — CR 701.12a: no part of the exchange occurs, and the resolver
/// must not panic on a missing object.
#[test]
fn avarice_totem_selfref_hostile_source_gone_yields_no_swap() {
    let mut state = GameState::new_two_player(42);
    // The Totem's id is referenced by the ability but never inserted into
    // `state.objects` — modeling "destroyed in response, the object no
    // longer exists" for the all-or-nothing gate at the object-lookup seam.
    let gone_totem = engine::types::identifiers::ObjectId(999);
    let target = create_object(
        &mut state,
        CardId(2),
        P1,
        "Target Permanent".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::ExchangeControl {
            target_a: TargetFilter::SelfRef,
            target_b: TargetFilter::Typed(TypedFilter::default()),
        },
        vec![TargetRef::Object(target)],
        gone_totem,
        P0,
    );
    let mut events = Vec::new();
    exchange_control::resolve(&mut state, &ability, &mut events).expect("must not panic");
    assert!(
        state.transient_continuous_effects.is_empty(),
        "CR 701.12a: the exchange can't be completed with a missing object ⇒ no part occurs"
    );
}

// ---------------------------------------------------------------------------
// V15 — CR 614.9 index + host agree for a context-ref recipient; the redirect
// position is untouched (round-1 M1 + M4)
// ---------------------------------------------------------------------------

/// V15: the en-Kor shape — `SelfRef` recipient + a declared redirect target.
/// The recipient surfaces NO slot; the redirect position surfaces exactly
/// one, and the shield hosts on the SOURCE (not the redirect target).
///
/// REVERT-FAILING (round-1 M1): deriving only the index from the context-ref
/// predicate (not the host) would host the shield on the redirect
/// destination instead of the source — `recipient_host` and
/// `recipient_consumes_slot` must come from the SAME predicate.
#[test]
fn damage_redirect_selfref_recipient_hosts_on_source_redirect_reads_the_only_slot() {
    let mut state = GameState::new_two_player(42);
    let en_kor = create_object(
        &mut state,
        CardId(1),
        P0,
        "Nomads en-Kor".to_string(),
        Zone::Battlefield,
    );
    let chosen = create_object(
        &mut state,
        CardId(2),
        P0,
        "Chosen Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&chosen)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    let ability = ResolvedAbility::new(
        Effect::CreateDamageReplacement {
            source_filter: None,
            combat_scope: None,
            target_filter: None,
            modification: None,
            redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
            redirect_amount: Some(PreventionAmount::Next(1)),
            redirect_object_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
            recipient_object_filter: Some(TargetFilter::SelfRef),
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
        },
        vec![TargetRef::Object(chosen)],
        en_kor,
        P0,
    );

    let slots = build_target_slots(&state, &ability).unwrap();
    assert_eq!(
        slots.len(),
        1,
        "only the redirect position surfaces a slot for a SelfRef recipient"
    );

    let mut events = Vec::new();
    create_damage_replacement::resolve(&mut state, &ability, &mut events).unwrap();
    let host = state.objects.get(&en_kor).unwrap();
    assert_eq!(
        host.replacement_definitions.len(),
        1,
        "the shield hosts on the SOURCE (en-Kor), not the redirect target"
    );
    assert_eq!(
        host.replacement_definitions[0].redirect_target,
        Some(TargetFilter::SpecificObject { id: chosen }),
        "the redirect target must still be the chosen creature (slot 0, unaffected by the \
         recipient's context-ref skip)"
    );
    assert!(
        state
            .objects
            .get(&chosen)
            .unwrap()
            .replacement_definitions
            .is_empty(),
        "no shield is hosted on the redirect target"
    );
}

/// HOSTILE (V15) — CR 614.9 + CR 400.7: a context-ref recipient whose
/// referent is GONE must install no shield at all.
///
/// Routing the recipient host through `targeting::resolved_targets` added a
/// way for `recipient_host` to be `None` that did not exist before (the old
/// code returned `ability.source_id` unconditionally for `SelfRef`). The
/// `None` fallback pushes the shield into `state.pending_damage_replacements`
/// WITHOUT stamping `valid_card: SelfRef`, and the en-Kor class carries no
/// `target_filter` either — so an unguarded fallback would install a shield
/// that redirects the next damage dealt to ANY object this turn.
///
/// `SelfRef` currency is only re-checked when the ability carries a
/// `trigger_source` context (`ResolvedAbility::self_ref_is_current`), which an
/// activated en-Kor ability does not — so this fixture stamps one explicitly
/// to reach the seam rather than relying on that incidental guard to keep it
/// unreachable.
///
/// REVERT-FAILING: without the `recipient_context_ref.is_some() &&
/// recipient_host.is_none()` guard, `pending_damage_replacements` gains one
/// unconstrained shield.
#[test]
fn damage_redirect_context_ref_recipient_with_a_dead_source_installs_no_shield() {
    let mut state = GameState::new_two_player(42);
    let en_kor = create_object(
        &mut state,
        CardId(1),
        P0,
        "Nomads en-Kor".to_string(),
        Zone::Battlefield,
    );
    let chosen = create_object(
        &mut state,
        CardId(2),
        P0,
        "Chosen Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&chosen)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    let mut ability = ResolvedAbility::new(
        Effect::CreateDamageReplacement {
            source_filter: None,
            combat_scope: None,
            target_filter: None,
            modification: None,
            redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
            redirect_amount: Some(PreventionAmount::Next(1)),
            redirect_object_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
            recipient_object_filter: Some(TargetFilter::SelfRef),
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
        },
        vec![TargetRef::Object(chosen)],
        en_kor,
        P0,
    );
    let source_context = engine::game::triggers::trigger_source_context_for_latch(
        &state,
        state.objects.get(&en_kor).expect("fixture source"),
    );
    ability.set_trigger_source_recursive(source_context);

    // The recipient (the source itself) is destroyed before the ability
    // resolves, so CR 400.7 currency fails and the context ref binds nothing.
    let mut move_events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, en_kor, Zone::Graveyard, &mut move_events);

    let mut events = Vec::new();
    create_damage_replacement::resolve(&mut state, &ability, &mut events).unwrap();

    assert!(
        state.pending_damage_replacements.is_empty(),
        "CR 614.9: a redirection whose original recipient is gone installs NO shield — an \
         unconstrained pending shield would redirect damage dealt to any object"
    );
    assert!(
        state
            .objects
            .get(&en_kor)
            .unwrap()
            .replacement_definitions
            .is_empty(),
        "nothing is hosted on the dead source either"
    );
    assert!(
        state
            .objects
            .get(&chosen)
            .unwrap()
            .replacement_definitions
            .is_empty(),
        "and nothing leaks onto the redirect destination"
    );
}

/// HOSTILE (V15 / M4): a synthetic fixture with BOTH a declared recipient AND
/// a declared redirect filter (no printed card carries both, but the
/// contract — recipient consumes slot 0, redirect reads slot 1 — must hold
/// regardless). This is the direct pin for the P2.1(b) unroll: applying the
/// recipient's `is_context_ref()` skip to a single shared loop instead of two
/// explicit positions would misalign this pair's indices.
#[test]
fn damage_redirect_declared_recipient_and_redirect_index_in_declaration_order() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        P0,
        "Synthetic Source".to_string(),
        Zone::Battlefield,
    );
    let recipient_creature = create_object(
        &mut state,
        CardId(2),
        P0,
        "Recipient".to_string(),
        Zone::Battlefield,
    );
    let redirect_creature = create_object(
        &mut state,
        CardId(3),
        P0,
        "Redirect Destination".to_string(),
        Zone::Battlefield,
    );
    for id in [recipient_creature, redirect_creature] {
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
    }

    let ability = ResolvedAbility::new(
        Effect::CreateDamageReplacement {
            source_filter: None,
            combat_scope: None,
            target_filter: None,
            modification: None,
            redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
            redirect_amount: None,
            redirect_object_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
            recipient_object_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
        },
        vec![
            TargetRef::Object(recipient_creature),
            TargetRef::Object(redirect_creature),
        ],
        source,
        P0,
    );

    let slots = build_target_slots(&state, &ability).unwrap();
    assert_eq!(slots.len(), 2, "both declared positions surface a slot");

    let mut events = Vec::new();
    create_damage_replacement::resolve(&mut state, &ability, &mut events).unwrap();
    assert!(
        state
            .objects
            .get(&recipient_creature)
            .unwrap()
            .replacement_definitions
            .len()
            == 1,
        "the shield hosts on the RECIPIENT (slot 0), not the source"
    );
    assert_eq!(
        state
            .objects
            .get(&recipient_creature)
            .unwrap()
            .replacement_definitions[0]
            .redirect_target,
        Some(TargetFilter::SpecificObject {
            id: redirect_creature
        }),
        "the redirect must read slot 1 (the SECOND declared target), not slot 0"
    );
}

// ---------------------------------------------------------------------------
// V16 — ChangeTargets binds a context-ref subject
// ---------------------------------------------------------------------------

/// V16: `Effect::ChangeTargets { target: TriggeringSource, .. }` binds the
/// retarget subject to the spell that triggered the current event, through
/// the same 4-tier `targeting::resolved_targets` authority — not just a
/// declared `ability.targets[0]`.
///
/// REVERT-FAILING: without `state.current_trigger_event` set,
/// `targeting::resolved_targets` cannot resolve `TriggeringSource`, so
/// `resolve` returns `Err(MissingParam(..))` even though a real stack entry
/// exists — this is the exact failure the pre-fix `ability.targets.first()`
/// read always produced for a context-ref subject (`ability.targets` is never
/// populated for `TriggeringSource`). Setting the trigger event is what makes
/// the second half of this test discriminate: reverting P2.4 would leave BOTH
/// halves failing identically.
#[test]
fn chimera_retarget_subject_binds_to_the_triggering_spell() {
    let mut state = GameState::new_two_player(42);
    let victim = create_object(
        &mut state,
        CardId(1),
        P0,
        "Victim Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&victim)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    let chimera = create_object(
        &mut state,
        CardId(2),
        P0,
        "Perplexing Chimera".to_string(),
        Zone::Battlefield,
    );
    let spell = create_object(
        &mut state,
        CardId(3),
        P1,
        "Doom Blade".to_string(),
        Zone::Stack,
    );
    let spell_ability = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        },
        vec![TargetRef::Object(victim)],
        spell,
        P1,
    );
    state.stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(3),
            ability: Some(Box::new(spell_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let retarget_ability = ResolvedAbility::new(
        Effect::ChangeTargets {
            target: TargetFilter::TriggeringSource,
            scope: RetargetScope::All,
            forced_to: None,
        },
        vec![],
        chimera,
        P0,
    );

    // REACH GUARD + revert-discrimination: without the trigger event bound,
    // the context-ref subject cannot resolve at all.
    let mut events = Vec::new();
    let err = change_targets::resolve(&mut state, &retarget_ability, &mut events)
        .expect_err("without a bound TriggeringSource there is no stack entry target");
    assert!(
        matches!(err, engine::types::ability::EffectError::MissingParam(_)),
        "expected MissingParam, got {err:?}"
    );

    // PRIMARY CLAIM: with the trigger event bound, the subject resolves to
    // the triggering spell and raises the retarget prompt for IT.
    state.current_trigger_event = Some(GameEvent::SpellCast {
        card_id: CardId(3),
        controller: P1,
        object_id: spell,
        cast_mana_value: None,
    });
    let mut events = Vec::new();
    change_targets::resolve(&mut state, &retarget_ability, &mut events)
        .expect("with the trigger event bound, the spell is a legal retarget subject");
    match state.waiting_for {
        WaitingFor::RetargetChoice {
            stack_entry_index, ..
        } => {
            assert_eq!(
                state.stack[stack_entry_index].id, spell,
                "the retarget prompt must target the TRIGGERING spell's stack entry"
            );
        }
        ref other => panic!("expected RetargetChoice for the triggering spell, got {other:?}"),
    }
}

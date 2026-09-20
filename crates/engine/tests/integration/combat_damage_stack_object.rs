//! Phase 3a: `StackEntryKind::CombatDamage` exists, is classified as neither a
//! spell nor an ability, and renders — while nothing constructs it in
//! production.
//!
//! The rules premise is the pre-M10 procedure (Classic Sixth Edition 1999
//! through Magic 2010, July 2009): all of a combat damage step's assignments go
//! on the stack as a single object, which is not a spell and not an ability and
//! therefore cannot be countered or targeted. Those historical rule numbers are
//! deliberately named only in prose — the current CR reuses 310 for Battles, so
//! citing them as `CR` annotations would point at the wrong rule.
//!
//! Every entry here is hand-built. That is the point of the phase: the variant
//! has no push authority yet, so a test is the only thing that can produce one.
//!
//! A restored state cannot seat one. An earlier revision left `resolve_top`
//! panicking on this kind, reasoning that nothing constructs one — but the
//! variant derives `Deserialize`, so a decoded `GameState` could seat one with
//! no push authority anywhere, turning that `unreachable!` into a crash path.
//! Resolving it off the stack as a no-op was worse: it would silently drop
//! pending damage this build cannot deal. The refusal therefore lives at the
//! persisted-admission boundary — `prepare_for_restore` rejects the kind
//! outright — which leaves `resolve_top` unreachable on two independent
//! grounds. Pinned by
//! `persisted_admission_refuses_combat_damage_and_still_admits_ordinary_states`.

use engine::game::derived_views::derive_views;
use engine::game::effects::copy_spell;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    CopyRetargetPermission, Effect, ResolvedAbility, TargetFilter, TypedFilter,
};
use engine::types::game_state::{
    AssignedCombatDamage, AssignedDamageRecipient, CombatDamageSubStep, GameState,
    PersistedGameState, PersistedRestoreError, PriorityYield, SpellCastRecord, StackEntry,
    StackEntryKind, StackResolutionEntryFence, WaitingFor, YieldTarget,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text (Scryfall, 2026-09-16).
const COUNTERSPELL: &str = "Counter target spell.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";

/// A combat-damage entry carrying one assignment, as the pushing phase will
/// build it: source and object recipient pinned to a CR 400.7 incarnation.
fn combat_damage_entry(
    entry_id: ObjectId,
    sub_step: CombatDamageSubStep,
    source: ObjectId,
    recipient: ObjectId,
    amount: u32,
) -> StackEntry {
    StackEntry {
        id: entry_id,
        source_id: entry_id,
        controller: P0,
        kind: StackEntryKind::CombatDamage {
            sub_step,
            assignments: vec![AssignedCombatDamage {
                source: ObjectIncarnationRef::of(source, 0),
                target: AssignedDamageRecipient::Object(ObjectIncarnationRef::of(recipient, 0)),
                amount,
            }],
        },
    }
}

/// A triggered-ability entry, used throughout as the paired positive control:
/// it is an ability, so every assertion that refuses a combat-damage entry has
/// something in the same run that it must still accept.
fn triggered_entry(entry_id: ObjectId, source: ObjectId, controller: PlayerId) -> StackEntry {
    let ability = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        },
        vec![],
        source,
        controller,
    );
    StackEntry {
        id: entry_id,
        source_id: source,
        controller,
        kind: StackEntryKind::TriggeredAbility {
            source_id: source,
            ability: Box::new(ability),
            condition: None,
            trigger_event: None,
            description: Some("Whenever this creature attacks, destroy target creature.".into()),
            source_name: "Control Trigger".into(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    }
}

/// An activated-ability entry — the second ability kind, so the ability row's
/// reach guard covers both halves of what a kindless counter accepts.
fn activated_entry(entry_id: ObjectId, source: ObjectId, controller: PlayerId) -> StackEntry {
    let ability = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        },
        vec![],
        source,
        controller,
    );
    StackEntry {
        id: entry_id,
        source_id: source,
        controller,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    }
}

/// Row 1a — no ABILITY-filter effect can target it.
///
/// This row is decided by the classification: `add_stack_abilities` pushes an
/// entry as a candidate with no `state.objects` lookup at all, so
/// `matches_stack_ability_kind` — i.e. `class()` — is the only gate. Reverting
/// `class()` to report an ability makes Squelch list the entry and this test
/// fails.
#[test]
fn ability_filters_cannot_target_a_combat_damage_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let blocker = scenario.add_vanilla(P0, 2, 2);
    let stifle = scenario
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let damage_id = ObjectId(9_001);
    let triggered_id = ObjectId(9_002);
    let activated_id = ObjectId(9_003);
    runner.state_mut().stack.push_back(combat_damage_entry(
        damage_id,
        CombatDamageSubStep::Regular,
        attacker,
        blocker,
        2,
    ));
    // TWO ability controls, deliberately: a kindless counter accepts both, so
    // the legal set is guaranteed to hold more than one candidate and target
    // selection must be raised rather than auto-assigned.
    runner
        .state_mut()
        .stack
        .push_back(triggered_entry(triggered_id, blocker, P1));
    runner
        .state_mut()
        .stack
        .push_back(activated_entry(activated_id, blocker, P1));

    let card_id = runner.state().objects[&stifle].card_id;
    let result = runner.act(engine::types::actions::GameAction::CastSpell {
        object_id: stifle,
        card_id,
        targets: vec![],
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });
    assert!(result.is_ok(), "Stifle must reach target selection");

    let legal = legal_targets_of(runner.state());
    // Reach guards: the ability enumeration ran and offered BOTH ability kinds.
    assert!(
        legal.contains(&engine::types::ability::TargetRef::Object(triggered_id)),
        "reach guard: the triggered-ability control must be offered"
    );
    assert!(
        legal.contains(&engine::types::ability::TargetRef::Object(activated_id)),
        "reach guard: the activated-ability control must be offered"
    );
    assert!(
        !legal.contains(&engine::types::ability::TargetRef::Object(damage_id)),
        "combat damage on the stack is not an ability and must not be targetable"
    );
}

/// Row 1b — no SPELL-filter effect can target it.
///
/// Deliberately claims **no** mutation. The exclusion here is structural rather
/// than class-decided: `stack_spell_entry_matches_filter` opens with a
/// `matches!(entry.kind, Spell { .. })` guard, and below it both spell paths
/// drop any entry with no `GameObject` — which a combat-damage entry never has.
/// Widening the kind guard therefore would not make the entry appear, so this
/// row proves the instrument fired instead: the control spell is offered, and
/// the entry was in `state.stack` for the enumeration that produced that offer.
#[test]
fn spell_filters_cannot_target_a_combat_damage_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let blocker = scenario.add_vanilla(P0, 2, 2);
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P0, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let victim = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    // A second victim, for the same reason as the ability row: two candidates
    // force target selection instead of a lone auto-assigned target.
    let victim2 = scenario
        .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let damage_id = ObjectId(9_101);
    runner.state_mut().stack.push_back(combat_damage_entry(
        damage_id,
        CombatDamageSubStep::Regular,
        attacker,
        blocker,
        2,
    ));
    // A real spell on the stack, as the control this row's negative is paired
    // with. Built through the object map so it has the `GameObject` a spell
    // entry is expected to have.
    for spell in [victim, victim2] {
        let spell_card = runner.state().objects[&spell].card_id;
        runner.state_mut().objects.get_mut(&spell).unwrap().zone =
            engine::types::zones::Zone::Stack;
        runner.state_mut().stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: P1,
            kind: StackEntryKind::Spell {
                card_id: spell_card,
                ability: None,
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    let card_id = runner.state().objects[&counterspell].card_id;
    let result = runner.act(engine::types::actions::GameAction::CastSpell {
        object_id: counterspell,
        card_id,
        targets: vec![],
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });
    assert!(result.is_ok(), "Counterspell must reach target selection");

    // Reach guard, read off the state the enumeration consumed: the entry was
    // on the stack when the legal set was produced.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == damage_id),
        "reach guard: the combat-damage entry was on the stack for this enumeration"
    );
    let legal = legal_targets_of(runner.state());
    assert!(
        legal.contains(&engine::types::ability::TargetRef::Object(victim)),
        "reach guard: the spell enumeration ran and offered the control spell"
    );
    assert!(
        !legal.contains(&engine::types::ability::TargetRef::Object(damage_id)),
        "combat damage on the stack is not a spell and must not be targetable"
    );
}

/// Every legal target the engine is currently offering, flattened across slots.
fn legal_targets_of(state: &GameState) -> Vec<engine::types::ability::TargetRef> {
    match &state.waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter().cloned())
            .collect(),
        other => panic!("expected target selection, got {other:?}"),
    }
}

/// Row 2 — the new state serializes, and a legacy incarnation payload still
/// loads through the existing compat shim.
#[test]
fn combat_damage_entry_round_trips_and_accepts_legacy_incarnations() {
    let entry = StackEntry {
        id: ObjectId(11),
        source_id: ObjectId(11),
        controller: P0,
        kind: StackEntryKind::CombatDamage {
            sub_step: CombatDamageSubStep::FirstStrike,
            assignments: vec![
                AssignedCombatDamage {
                    source: ObjectIncarnationRef::of(ObjectId(3), 7),
                    target: AssignedDamageRecipient::Object(ObjectIncarnationRef::of(
                        ObjectId(4),
                        2,
                    )),
                    amount: 3,
                },
                AssignedCombatDamage {
                    source: ObjectIncarnationRef::of(ObjectId(5), 1),
                    target: AssignedDamageRecipient::Player(P1),
                    amount: 2,
                },
            ],
        },
    };

    let json = serde_json::to_string(&entry).expect("serializes");
    let back: StackEntry = serde_json::from_str(&json).expect("round-trips");
    assert_eq!(back, entry, "both recipient arms must survive a round trip");

    // A legacy payload stored a bare `ObjectId` where the incarnation pair now
    // lives; `ObjectIncarnationRefCompat` is what keeps it loading.
    let legacy = json.replace(r#"{"object_id":3,"incarnation":7}"#, "3");
    assert_ne!(
        legacy, json,
        "the legacy rewrite must actually change the payload"
    );
    let from_legacy: StackEntry =
        serde_json::from_str(&legacy).expect("a legacy bare-ObjectId source still deserializes");
    assert!(matches!(
        from_legacy.kind,
        StackEntryKind::CombatDamage { .. }
    ));
}

/// Row 3 — the engine owns the label; the client renders `kind_label` and
/// derives nothing.
#[test]
fn engine_supplies_the_combat_damage_label_for_both_sub_steps() {
    let mut runner = GameScenario::new().build();
    let first = ObjectId(9_201);
    let regular = ObjectId(9_202);
    let control = ObjectId(9_203);
    let source = ObjectId(1);
    runner.state_mut().stack.push_back(combat_damage_entry(
        first,
        CombatDamageSubStep::FirstStrike,
        source,
        source,
        1,
    ));
    runner.state_mut().stack.push_back(combat_damage_entry(
        regular,
        CombatDamageSubStep::Regular,
        source,
        source,
        1,
    ));
    runner
        .state_mut()
        .stack
        .push_back(triggered_entry(control, source, P0));

    let views = derive_views(runner.state(), Some(P0));
    let label = |id: ObjectId| views.stack_entry_details[&id].kind_label.clone();
    assert_eq!(label(first), "Combat damage — first strike");
    assert_eq!(label(regular), "Combat damage");
    // Sibling control: the existing labels are untouched.
    assert_eq!(label(control), "Triggered ability");

    let detail = &views.stack_entry_details[&regular];
    assert!(detail.provenance.is_none(), "not a synthesized trigger");
    assert!(
        detail.targets.is_empty(),
        "assignment lines land with the pushing phase"
    );
}

/// Row 4 — combat-damage entries never coalesce in the stack display, while
/// genuinely identical triggers still do.
#[test]
fn combat_damage_entries_never_coalesce_but_identical_triggers_still_do() {
    let mut runner = GameScenario::new().build();
    let source = ObjectId(1);
    let a = ObjectId(9_301);
    let b = ObjectId(9_302);
    for id in [a, b] {
        runner.state_mut().stack.push_back(combat_damage_entry(
            id,
            CombatDamageSubStep::Regular,
            source,
            source,
            1,
        ));
    }
    let t1 = ObjectId(9_303);
    let t2 = ObjectId(9_304);
    for id in [t1, t2] {
        runner
            .state_mut()
            .stack
            .push_back(triggered_entry(id, source, P0));
    }

    let views = derive_views(runner.state(), Some(P0));
    let damage_groups = views
        .stack_display_groups
        .iter()
        .filter(|group| group.member_ids.iter().any(|id| *id == a || *id == b))
        .count();
    assert_eq!(
        damage_groups, 2,
        "each combat damage step is its own object and must not be coalesced"
    );
    // Positive control: grouping still works for the entries it is meant for.
    let trigger_group = views
        .stack_display_groups
        .iter()
        .find(|group| group.member_ids.contains(&t1))
        .expect("the identical triggers form a group");
    assert_eq!(
        trigger_group.count, 2,
        "identical triggers must still coalesce, or this test proves nothing"
    );
}

/// Row 5 — the resolution fence captures the new kind rather than losing it.
#[test]
fn the_resolution_fence_captures_a_combat_damage_entry() {
    let entry = combat_damage_entry(
        ObjectId(21),
        CombatDamageSubStep::FirstStrike,
        ObjectId(3),
        ObjectId(4),
        2,
    );
    let fence = StackResolutionEntryFence::capture(&entry);
    let json = serde_json::to_string(&fence).expect("the fence serializes");
    assert!(
        json.contains("CombatDamage"),
        "the fence must record the kind, not erase it: {json}"
    );

    // Sibling control: a keyword-action-free ability entry still captures too.
    let control = triggered_entry(ObjectId(22), ObjectId(3), P0);
    let control_fence = StackResolutionEntryFence::capture(&control);
    assert_eq!(control_fence.entry_id, ObjectId(22));
}

/// Row 6 — a player can never pre-yield priority to combat damage (CR 117.3d).
///
/// Routed through the auto-pass recommendation, which consults
/// `is_priority_yielded` with **no** controller conjunct — unlike the session
/// gate, which short-circuits on `top.controller != player` and would make this
/// row decided by the entry's seat. The two boards below differ only in the top
/// stack entry.
#[test]
fn combat_damage_is_never_priority_yielded() {
    // NOTE: `combat_damage_entry` sets `source_id` to the ENTRY id (a
    // combat-damage object has no single source permanent), so the yield must
    // be keyed on THAT id for the two entries to be compared on equal footing.
    // An earlier revision claimed the two "shared a source" while keying the
    // yield on the control's source — which made the negative pass for the
    // wrong reason.
    let entry = combat_damage_entry(
        ObjectId(31),
        CombatDamageSubStep::Regular,
        ObjectId(3),
        ObjectId(4),
        2,
    );
    // The control's source is the entry's OWN source id, so one stored yield
    // genuinely covers both.
    let control = triggered_entry(ObjectId(32), ObjectId(31), P0);

    let mut runner = GameScenario::new().build();
    let state = runner.state_mut();
    // Store a real yield keyed to the shared source, so the control below can
    // actually be yielded. Without this the negative passes vacuously — an
    // empty yield list refuses everything.
    state.priority_yields.push(PriorityYield {
        player: P0,
        target: YieldTarget::ThisObject {
            source_id: ObjectId(31),
            incarnation: None,
            trigger_description: None,
        },
    });

    // Positive control FIRST: the instrument fires — an ability from that
    // source is yielded under exactly this stored yield.
    assert!(
        state.is_priority_yielded(P0, &control),
        "reach guard: the stored yield must match the control ability"
    );
    // The combat-damage entry shares that source and that seat, and is still
    // never yielded, because it is not an ability (CR 117.3d).
    assert!(
        !state.is_priority_yielded(P0, &entry),
        "combat damage is not an ability and can never be yielded"
    );
}

/// An untargeted copy effect must not duplicate a combat-damage entry.
///
/// CR 707.10 copies a spell or ability, and combat damage is neither. The
/// dangerous route is the UNTARGETED one: with no target in the ability,
/// `copy_source_entry` falls through to `state.stack.last()` without ever
/// consulting a target filter — so rows 1a/1b's targeting-legality argument
/// does not cover it. `stack_entry_cant_be_copied` is what refuses it, and
/// `copy_spell::resolve` returns `Ok` having copied nothing.
///
/// REVERT PROBE: delete the `CombatDamage` arm at the head of
/// `stack_entry_cant_be_copied` and this test reds — the fallback picks the
/// top-of-stack combat-damage entry and pushes a duplicate.
#[test]
fn an_untargeted_copy_cannot_duplicate_a_combat_damage_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();

    let damage_id = ObjectId(9_501);
    runner.state_mut().stack.push_back(combat_damage_entry(
        damage_id,
        CombatDamageSubStep::Regular,
        source,
        source,
        2,
    ));

    // Reach guard: the untargeted fallback reads `state.stack.last()`, so the
    // combat-damage entry must genuinely be the top of the stack.
    assert!(
        matches!(
            runner.state().stack.back().map(|entry| &entry.kind),
            Some(StackEntryKind::CombatDamage { .. })
        ),
        "reach guard: the fallback's `state.stack.last()` is the combat-damage entry"
    );
    let before = runner.state().stack.len();

    // No targets — this is the untargeted form the gate has to catch.
    let copy_ability = ResolvedAbility::new(
        Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        },
        vec![],
        ObjectId(9_502),
        P0,
    );
    let mut events = Vec::new();
    copy_spell::resolve(runner.state_mut(), &copy_ability, &mut events)
        .expect("CR 707.10: the copy effect resolves, having copied nothing");

    assert_eq!(
        runner.state().stack.len(),
        before,
        "no duplicate combat-damage entry may be pushed"
    );

    // Positive control, same call shape on the same board: an ordinary ability
    // entry on top IS copied, so the refusal above is about this kind and not
    // about `resolve` refusing everything.
    runner
        .state_mut()
        .stack
        .push_back(triggered_entry(ObjectId(9_503), source, P0));
    let before_control = runner.state().stack.len();
    let mut control_events = Vec::new();
    copy_spell::resolve(runner.state_mut(), &copy_ability, &mut control_events)
        .expect("copying an ordinary ability entry must succeed");
    assert_eq!(
        runner.state().stack.len(),
        before_control + 1,
        "reach guard: an ordinary ability entry is still copyable"
    );
}

/// A persisted state carrying combat damage must be REFUSED at admission.
///
/// Decoding the type and admitting it as a playable game are separate
/// contracts. The variant is serde-decodable so later phases can round-trip
/// it, but no phase before the pushing one can deal its assignments — so a
/// restored state holding one has pending damage this build cannot resolve.
/// Admitting it would either crash resolution or silently drop that damage.
/// `prepare_for_restore` is the single chokepoint every production restore
/// funnels through (WASM `prepare_restored_game_state`, `server-core`'s
/// `from_persisted`, offline tooling), so the refusal lives there.
///
/// Covers BOTH persisted forms with a populated payload, and pairs each with a
/// supported-entry positive control proving ordinary states still restore.
///
/// REVERT PROBE: delete the `UnsupportedStackObject` guard in
/// `prepare_for_restore` and both rejection arms below fail.
#[test]
fn persisted_admission_refuses_combat_damage_and_still_admits_ordinary_states() {
    // A populated payload: a real assignment, not an empty entry.
    let seated = |runner: &mut engine::game::scenario::GameRunner| {
        runner.state_mut().stack.push_back(combat_damage_entry(
            ObjectId(9_601),
            CombatDamageSubStep::Regular,
            ObjectId(1),
            ObjectId(2),
            3,
        ));
    };

    for raw in [true, false] {
        let form = if raw { "Raw" } else { "Trusted" };

        // Positive control FIRST: the same form, same board, carrying an
        // ORDINARY stack entry, must still restore. Without this the rejection
        // below could pass because the boundary refuses everything.
        //
        // An ACTIVATED ability deliberately, not a triggered one. Decoding runs
        // `validate_trigger_firing_coherence`, which demands that every
        // `TriggeredAbility` stack entry carry a matching row in
        // `stack_trigger_firings` — a `pub(crate)` field an integration test
        // cannot seat. A triggered control therefore fails to DECODE and never
        // reaches the admission boundary this test exists to guard. That loop
        // skips every non-triggered kind, so an activated entry is an ordinary
        // supported object with one fewer precondition.
        let mut ok_runner = GameScenario::new().build();
        ok_runner
            .state_mut()
            .stack
            .push_back(activated_entry(ObjectId(9_602), ObjectId(1), P0));
        let ok_state = ok_runner.state().clone();
        let ok_persisted = if raw {
            PersistedGameState::Raw(Box::new(ok_state))
        } else {
            PersistedGameState::capture(ok_state)
        };
        let ok_json = serde_json::to_string(&ok_persisted).expect("control serializes");
        let ok_decoded: PersistedGameState =
            serde_json::from_str(&ok_json).expect("control decodes");
        assert!(
            ok_decoded.into_game_state().is_ok(),
            "reach guard ({form}): an ordinary stack entry must still be admitted"
        );

        // The combat-damage payload must be refused by the same boundary.
        let mut bad_runner = GameScenario::new().build();
        seated(&mut bad_runner);
        let bad_state = bad_runner.state().clone();
        let bad_persisted = if raw {
            PersistedGameState::Raw(Box::new(bad_state))
        } else {
            PersistedGameState::capture(bad_state)
        };
        let bad_json = serde_json::to_string(&bad_persisted).expect("payload serializes");
        let bad_decoded: PersistedGameState =
            serde_json::from_str(&bad_json).expect("payload still DECODES — that contract is kept");
        match bad_decoded.into_game_state() {
            Err(PersistedRestoreError::UnsupportedStackObject(_)) => {}
            Err(other) => panic!("({form}) refused for the wrong reason: {other:?}"),
            Ok(_) => panic!("({form}) a state carrying unresolvable combat damage was admitted"),
        }
    }
}

/// Persisted admission also owns the entry already popped for resolution.
/// A non-Priority prompt keeps terminal-rest recovery from masking this gate.
#[test]
fn persisted_admission_refuses_paused_combat_damage_carriers() {
    for raw in [true, false] {
        for unsupported in [false, true] {
            let mut runner = GameScenario::new().build();
            let entry = if unsupported {
                combat_damage_entry(
                    ObjectId(9_603),
                    CombatDamageSubStep::Regular,
                    ObjectId(1),
                    ObjectId(2),
                    3,
                )
            } else {
                activated_entry(ObjectId(9_604), ObjectId(1), P0)
            };
            runner.state_mut().resolving_stack_entry = Some(entry);
            // Synthetic persisted boundary fixture: both payload kinds use the
            // same paused prompt; this does not claim a Phase 3c damage resolver.
            runner.state_mut().waiting_for = WaitingFor::ScryChoice {
                player: P0,
                cards: vec![],
            };
            let state = runner.state().clone();
            let persisted = if raw {
                PersistedGameState::Raw(Box::new(state))
            } else {
                PersistedGameState::capture(state)
            };
            let json = serde_json::to_string(&persisted).expect("carrier serializes");
            let decoded: PersistedGameState = serde_json::from_str(&json).expect("carrier decodes");
            let restored = decoded.into_game_state();
            if unsupported {
                assert!(
                    matches!(
                        restored,
                        Err(PersistedRestoreError::UnsupportedStackObject(_))
                    ),
                    "raw={raw}: reject the unsupported resolving carrier at admission"
                );
            } else {
                let restored = restored.expect("ordinary paused carrier still restores");
                assert!(restored.stack.is_empty());
                assert!(matches!(
                    restored.waiting_for,
                    WaitingFor::ScryChoice { .. }
                ));
                assert!(matches!(
                    restored
                        .resolving_stack_entry
                        .as_ref()
                        .map(|entry| &entry.kind),
                    Some(StackEntryKind::ActivatedAbility { .. })
                ));
            }
        }
    }
}

/// Row 7 — the storm count ignores a combat-damage entry rather than treating
/// it as a shadowing stack object.
#[test]
fn storm_count_ignores_a_combat_damage_entry() {
    let mut runner = GameScenario::new().build();
    let source = ObjectId(1);
    let damage = ObjectId(9_401);
    runner.state_mut().stack.push_back(combat_damage_entry(
        damage,
        CombatDamageSubStep::Regular,
        source,
        source,
        1,
    ));

    // A NONZERO baseline is the point: with an all-zero ledger, an arm that
    // wrongly returned `Some(0)` would be indistinguishable from the correct
    // fall-through. Seed the ledger `storm_count` folds so the expected value
    // is nonzero and a zero override is visible.
    let cast_records = runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .map_or(0, |records| records.len());
    assert_eq!(
        cast_records, 0,
        "fixture assumption: the ledger starts empty before seeding"
    );
    runner.state_mut().spells_cast_this_turn_by_player.insert(
        P0,
        vec![SpellCastRecord::default(); 2].into_iter().collect(),
    );

    let with_entry = derive_views(runner.state(), Some(P0)).storm_count;
    assert_eq!(
        with_entry, 2,
        "the combat-damage entry must not shadow the seeded cast count"
    );

    // Differential control: removing the entry must not change the answer.
    runner.state_mut().stack.clear();
    let without_entry = derive_views(runner.state(), Some(P0)).storm_count;
    assert_eq!(
        with_entry, without_entry,
        "a combat-damage entry must not shadow or alter the storm count"
    );
}

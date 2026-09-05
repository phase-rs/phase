//! Phase 1 of the Perplexing Chimera run — "a spell's controller is a derived
//! value" (U1a/U1b/U1c stack-controller seed, U2 live-controller consumers, U3
//! live-controller display projection).
//!
//! Covers Verification Matrix rows V1 .. V11 (plan-r6 §Verification Matrix,
//! Stage-1 rows). Stage-2 rows (V12..V18, context-ref slot hygiene + exchange
//! control of a spell) are Phase 2 and are NOT covered here.
//!
//! Two seams are exercised directly via their exact production functions
//! rather than through a full alternative-cost / target-selection UI replay,
//! because the claims under test do not depend on cast authorization, mana
//! payment, or target-selection sequencing — only on the zone transition +
//! stack arrival (`zones::move_to_zone`, `layers::evaluate_layers`,
//! `layers::flush_layers`) and the resolution routing (`stack::resolve_top`):
//!   * `zones::move_to_zone` is called directly for the origin-zone rows
//!     (V3, V3b) instead of reconstructing `CastingPermission::ExileWithAltCost`
//!     grants and driving a full `GameAction::CastSpell`.
//!   * `stack::resolve_top` is called directly against a hand-built
//!     `StackEntry` for the resolution-routing rows (V5, V5b, V5c, V6, V7,
//!     V10) instead of driving mutate/cipher/epic/paradigm through their full
//!     alternative-cost cast UIs.
//!
//! V9 (crime detection, `casting::targets_commit_crime` is `pub(crate)`) and
//! the client-observable half of V11 are driven through the real
//! `GameRunner`/`GameScenario` cast pipeline, since those seams are only
//! reachable that way from an external integration test.

use std::collections::BTreeSet;

use engine::game::ability_utils::parent_target_controller;
use engine::game::elimination::eliminate_player;
use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::perf_counters;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::stack::{resolve_top, stack_object_controller};
use engine::game::targeting::{find_legal_targets, resolved_targets};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    ContinuousModification, ControllerRef, Duration, Effect, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastingVariant, GameState, LayersDirty, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn new_state() -> GameState {
    GameState::new_two_player(42)
}

/// A spell object sitting directly in `zone`, owned by `owner`, with base
/// characteristics initialized (so the layers reset loop's
/// `sync_missing_base_characteristics` guard is satisfied per P1.12's setup
/// trap note).
fn spell_object(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    card_num: u64,
    zone: Zone,
) -> ObjectId {
    let id = create_object(state, CardId(card_num), owner, name.to_string(), zone);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    obj.keywords = vec![Keyword::Flying];
    obj.base_keywords = vec![Keyword::Flying];
    obj.base_characteristics_initialized = true;
    id
}

fn creature_on_battlefield(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    card_num: u64,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_num),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    obj.base_characteristics_initialized = true;
    id
}

/// Push a `Spell` stack entry for `id`, with `controller` as the CR 112.2
/// by-default controller. `ability` carries the declared targets (and, for
/// mutate/cipher/epic/paradigm fixtures, the keyword-driven on-resolution
/// hooks read the object's own keywords, not the ability).
fn push_spell_entry(
    state: &mut GameState,
    id: ObjectId,
    controller: PlayerId,
    ability: Option<ResolvedAbility>,
    casting_variant: CastingVariant,
) {
    let card_id = state.objects[&id].card_id;
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: ability.map(Box::new),
            casting_variant,
            actual_mana_spent: 0,
        },
    });
}

/// CR 613.1b: install an `UntilEndOfTurn` `ChangeController` continuous effect
/// naming `id` as its sole recipient, controlled by `thief`. Marks layers
/// dirty (`GameState::add_transient_continuous_effect` marks `Full`
/// internally) but does not itself flush — callers call `evaluate_layers` /
/// `flush_layers` afterward.
fn install_steal(state: &mut GameState, source_id: ObjectId, thief: PlayerId, id: ObjectId) {
    state.add_transient_continuous_effect(
        source_id,
        thief,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id },
        vec![ContinuousModification::ChangeController],
        None,
    );
}

/// `GameState::new_two_player` seeds no libraries — a `Draw` effect against an
/// empty library is a silent no-op (CR 121.3 + CR 704.5b's loss-by-drawing SBA
/// is not checked by these tests), which would make a hand-growth assertion
/// pass or fail vacuously regardless of WHO drew. Seed both players so either
/// can draw.
fn seed_libraries(state: &mut GameState, card_num_base: u64) {
    for (i, player) in [P0, P1].into_iter().enumerate() {
        for j in 0..3u64 {
            create_object(
                state,
                CardId(card_num_base + (i as u64) * 10 + j),
                player,
                format!("Library Card {i}-{j}"),
                Zone::Library,
            );
        }
    }
}

fn draw_ability(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source_id,
        controller,
    )
}

// ---------------------------------------------------------------------------
// V1 — a layer-2 control change on a stack object is re-derived each pass
// ---------------------------------------------------------------------------

/// CR 613.1b + CR 112.2: `until_eot_control_change_on_a_spell_reverts_at_cleanup`.
/// REVERT-FAILING: delete the `obj.controller = base` seed line in
/// `layers::evaluate_layers` and this stays the thief forever (STICKY).
#[test]
fn until_eot_control_change_on_a_spell_reverts_at_cleanup() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Sticky Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);

    let effect_id = {
        let before = state.transient_continuous_effects.len();
        install_steal(&mut state, source, thief, spell);
        assert_eq!(
            state.transient_continuous_effects.len(),
            before + 1,
            "install_steal must add exactly one transient effect"
        );
        state.transient_continuous_effects.back().unwrap().id
    };

    evaluate_layers(&mut state);
    // REACH GUARD: the effect actually applied before asserting the revert.
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(
        stack_object_controller(&state, &entry),
        thief,
        "the ChangeController effect must have applied before cleanup"
    );

    // Simulate cleanup-step expiry: the effect is removed, the next pass reverts.
    state
        .transient_continuous_effects
        .retain(|e| e.id != effect_id);
    evaluate_layers(&mut state);
    assert_eq!(
        stack_object_controller(&state, &entry),
        caster,
        "CR 112.2: after the control effect ends, the spell reverts to its by-default caster"
    );
}

/// HOSTILE (V1): empty stack ⇒ the reset loop no-ops without panicking.
#[test]
fn empty_stack_layers_pass_does_not_panic() {
    let mut state = new_state();
    evaluate_layers(&mut state);
}

/// HOSTILE (V1): a `StackEntry` with no `state.objects` row (an ability entry)
/// ⇒ `objects.get_mut` misses, no panic, and the accessor still answers
/// `entry.controller` per CR 113.8.
#[test]
fn ability_stack_entry_with_no_object_row_is_safe_to_reset() {
    let mut state = new_state();
    let source = creature_on_battlefield(&mut state, P0, "Ability Source", 1);
    let ability_id = ObjectId(9999); // no matching `state.objects` row
    state.stack.push_back(StackEntry {
        id: ability_id,
        source_id: source,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ResolvedAbility::new(
                Effect::unimplemented("test", "ability entry"),
                vec![],
                source,
                P0,
            )),
        },
    });
    evaluate_layers(&mut state); // must not panic
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(
        stack_object_controller(&state, &entry),
        P0,
        "an ability entry with no object row answers entry.controller (CR 113.8)"
    );
}

/// HOSTILE (V1, r5/§F-1): an entry on the stack whose object is still in its
/// ORIGIN zone (the CR 601.2a announcement window) must NOT be stamped with a
/// controller — CR 109.4 gives a controller only to objects on the stack or
/// battlefield. REVERT-FAILING for the guard: delete `if obj.zone ==
/// Zone::Stack` and the exiled, opponent-owned card is stamped with the
/// caster. REACH GUARD: a genuine `Zone::Stack` sibling in the same pass WAS
/// seeded, so "unchanged" cannot pass because nothing happened.
#[test]
fn stack_entry_whose_object_is_still_in_its_origin_zone_is_not_stamped() {
    let mut state = new_state();
    // The entry is on the stack (CR 601.2a announcement), but the object is
    // still physically in Exile — the pre-finalization window.
    let announced = spell_object(&mut state, P1, "Mid-Announce Spell", 1, Zone::Exile);
    push_spell_entry(&mut state, announced, P0, None, CastingVariant::Normal);

    // A genuine Zone::Stack sibling in the same pass.
    let sibling = spell_object(&mut state, P1, "Sibling Spell", 2, Zone::Stack);
    push_spell_entry(&mut state, sibling, P1, None, CastingVariant::Normal);

    evaluate_layers(&mut state);

    assert_eq!(
        state.objects[&announced].controller, P1,
        "an object still in its origin zone (Exile) must not be given a controller (CR 109.4)"
    );
    assert_eq!(
        state.objects[&sibling].controller, P1,
        "REACH GUARD: the genuine Zone::Stack sibling was seeded in the same pass"
    );
}

// ---------------------------------------------------------------------------
// V1b — the seed maintains the popped resolving entry too (M2)
// ---------------------------------------------------------------------------

/// CR 608.2m: `resolving_stack_entry_is_reset_by_the_stack_seed`.
/// REVERT-FAILING: delete the `.chain(..)` and the object is never visited.
#[test]
fn resolving_stack_entry_is_reset_by_the_stack_seed() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Resolving Spell", 1, Zone::Stack);
    let entry = StackEntry {
        id: spell,
        source_id: spell,
        controller: caster,
        kind: StackEntryKind::Spell {
            card_id: state.objects[&spell].card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    };
    // Popped: NOT in state.stack, but recorded as the resolving entry.
    state.resolving_stack_entry = Some(entry);
    state.objects.get_mut(&spell).unwrap().controller = thief; // stale value

    // A live-stack sibling in the same pass.
    let sibling = spell_object(&mut state, caster, "Sibling Spell", 2, Zone::Stack);
    push_spell_entry(&mut state, sibling, caster, None, CastingVariant::Normal);
    state.objects.get_mut(&sibling).unwrap().controller = thief; // stale too

    evaluate_layers(&mut state);

    assert_eq!(
        state.objects[&spell].controller, caster,
        "the resolving_stack_entry chain must reset the popped entry's object too"
    );
    assert_eq!(
        state.objects[&sibling].controller, caster,
        "REACH GUARD: the live-stack sibling was reset in the same pass"
    );
}

/// HOSTILE (V1b): resolving entry whose object is Zone::Graveyard (the shape
/// measured at a real pause) ⇒ the `obj.zone == Zone::Stack` guard excludes it.
#[test]
fn resolving_stack_entry_in_graveyard_is_excluded() {
    let mut state = new_state();
    let spell = spell_object(&mut state, P1, "Graveyard Resolving", 1, Zone::Graveyard);
    state.resolving_stack_entry = Some(StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: state.objects[&spell].card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    state.objects.get_mut(&spell).unwrap().controller = P0;
    evaluate_layers(&mut state);
    assert_eq!(
        state.objects[&spell].controller, P0,
        "a resolving entry whose object is Zone::Graveyard must not be touched"
    );
}

/// HOSTILE (V1b): resolving entry ALSO present in `state.stack` ⇒ the
/// `!any(live)` guard prevents a double visit (no panic, no double-reset).
#[test]
fn resolving_stack_entry_also_live_is_not_double_visited() {
    let mut state = new_state();
    let spell = spell_object(&mut state, P1, "Double Entry", 1, Zone::Stack);
    let entry = StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: state.objects[&spell].card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    };
    state.stack.push_back(entry.clone());
    state.resolving_stack_entry = Some(entry);
    evaluate_layers(&mut state); // must not panic
    assert_eq!(state.objects[&spell].controller, P1);
}

/// HOSTILE (V1b): `resolving_stack_entry == None` ⇒ `Option::iter` yields nothing.
#[test]
fn no_resolving_stack_entry_is_a_no_op_for_the_chain() {
    let mut state = new_state();
    assert!(state.resolving_stack_entry.is_none());
    evaluate_layers(&mut state); // must not panic
}

// ---------------------------------------------------------------------------
// V2 — the BASE stays the caster on the stack
// ---------------------------------------------------------------------------

/// CR 112.2: `entry_controller_is_never_written_by_the_seed`.
#[test]
fn entry_controller_is_never_written_by_the_seed() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Base Stays Caster", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    let base_controller_before = state.objects[&spell].base_controller;

    evaluate_layers(&mut state);
    // REACH GUARD: obj.controller DID change.
    assert_eq!(state.objects[&spell].controller, thief);
    // Two full passes back-to-back — idempotent.
    evaluate_layers(&mut state);
    assert_eq!(state.objects[&spell].controller, thief);

    let entry = state.stack.back().unwrap();
    assert_eq!(
        entry.controller, caster,
        "StackEntry.controller (the CR 112.2 by-default value) must never be written by the seed"
    );
    assert_eq!(
        state.objects[&spell].base_controller, base_controller_before,
        "the seed must never touch obj.base_controller"
    );
}

// ---------------------------------------------------------------------------
// V3 / V3b — the CR 601.2a arrival mark (U1c) feeding the CR 112.2 seed (U1a)
// ---------------------------------------------------------------------------

/// CR 601.2a + CR 611.2f + CR 112.2: `spell_cast_from_a_zone_its_caster_does_not_own_is_controlled_by_the_caster`.
/// The Gonti class: a card the OTHER seat owns, cast from Exile. REVERT-FAILING
/// (two legs, both against real edits): (i) revert U1c's `to == Zone::Stack`
/// disjunct ⇒ no pass runs, the seed never executes, and the read answers the
/// OWNER; (ii) revert U1a's `obj.controller = base` line ⇒ the pass runs but
/// writes nothing, same owner answer.
#[test]
fn spell_cast_from_a_zone_its_caster_does_not_own_is_controlled_by_the_caster() {
    let mut state = new_state();
    let owner = P1;
    let caster = P0;
    let spell = spell_object(&mut state, owner, "Gonti Class Spell", 1, Zone::Exile);
    // Force the lattice Clean so the move itself is the only thing that can mark it.
    state.layers_dirty = LayersDirty::Clean;

    let mut events = Vec::new();
    move_to_zone(&mut state, spell, Zone::Stack, &mut events);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    flush_layers(&mut state);

    // REACH GUARD: the spell actually reached the stack under the caster.
    assert_eq!(state.stack.len(), 1, "the spell must be on the stack");
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(entry.controller, caster);

    assert_eq!(
        stack_object_controller(&state, &entry),
        caster,
        "CR 112.2: the caster is the by-default controller, not the owner"
    );
    assert_ne!(
        state.objects[&spell].owner, caster,
        "the fixture must be owner != caster (the Gonti class) or this row measures nothing"
    );
    assert_eq!(state.objects[&spell].owner, owner);
}

/// SIBLING (V3): ordinary hand cast (owner == caster) is unchanged — the
/// positive control proving the row measures the ORIGIN ZONE.
#[test]
fn ordinary_hand_cast_owner_equals_caster_is_unaffected() {
    let mut state = new_state();
    let caster = P1;
    let spell = spell_object(&mut state, caster, "Ordinary Hand Cast", 1, Zone::Hand);
    state.layers_dirty = LayersDirty::Clean;
    let mut events = Vec::new();
    move_to_zone(&mut state, spell, Zone::Stack, &mut events);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    flush_layers(&mut state);
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(stack_object_controller(&state, &entry), caster);
    assert_eq!(state.objects[&spell].owner, caster);
}

/// V3b — `a_cast_from_each_origin_zone_seeds_the_stack_objects_controller`.
/// Four named sub-cases, matching plan-r6's rebuilt row (review-r5/B-1).
mod origin_zone_arrival_mark {
    use super::*;

    /// ① EXILE — owner != caster (the Gonti class). REVERT-FAILING: delete
    /// `to == Zone::Stack` from the disjunction and no pass runs for an exile
    /// origin, so the seed never executes and the assertion reads the OWNER.
    /// SIBLING (Class B'): a live UntilEndOfTurn AddKeyword effect filtered to
    /// SpecificObject{id} must also be APPLIED — a second, independent
    /// revert-failing leg on the same disjunct, pinning that U1c's benefit is
    /// not controller-specific.
    #[test]
    fn exile_origin_seeds_controller_and_keeps_the_pre_existing_keyword_grant() {
        let mut state = new_state();
        let owner = P1;
        let caster = P0;
        let spell = spell_object(&mut state, owner, "Exile Origin Spell", 1, Zone::Exile);
        let source = creature_on_battlefield(&mut state, caster, "Keyword Source", 2);
        state.add_transient_continuous_effect(
            source,
            caster,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: spell },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }],
            None,
        );
        state.layers_dirty = LayersDirty::Clean;

        let mut events = Vec::new();
        move_to_zone(&mut state, spell, Zone::Stack, &mut events);
        push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
        flush_layers(&mut state);

        let entry = state.stack.back().unwrap().clone();
        assert_eq!(stack_object_controller(&state, &entry), caster);
        assert_ne!(state.objects[&spell].owner, caster);
        assert!(
            state.objects[&spell].keywords.contains(&Keyword::Trample),
            "the pre-existing CR 613.1 keyword grant must still apply for an exile origin"
        );
    }

    /// ② GRAVEYARD — owner != caster, mirroring Memory Plunder's opponent-
    /// graveyard free cast (`cast_from_zone.rs::opponent_graveyard_free_cast_moves_directly_to_stack`).
    #[test]
    fn graveyard_origin_seeds_controller() {
        let mut state = new_state();
        let owner = P1;
        let caster = P0;
        let spell = spell_object(
            &mut state,
            owner,
            "Graveyard Origin Spell",
            1,
            Zone::Graveyard,
        );
        state.layers_dirty = LayersDirty::Clean;

        let mut events = Vec::new();
        move_to_zone(&mut state, spell, Zone::Stack, &mut events);
        push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
        flush_layers(&mut state);

        let entry = state.stack.back().unwrap().clone();
        assert_eq!(stack_object_controller(&state, &entry), caster);
        assert_ne!(state.objects[&spell].owner, caster);
    }

    /// ③ COMMAND — MECHANISM assertion: a commander is cast by its own owner
    /// by construction, so no controller equality here can discriminate;
    /// assert instead that the pass RAN (`layers_full_eval` incremented).
    #[test]
    fn command_zone_origin_runs_a_full_pass() {
        let mut state = new_state();
        let caster = P0;
        let spell = spell_object(&mut state, caster, "Commander", 1, Zone::Command);
        state.layers_dirty = LayersDirty::Clean;
        perf_counters::reset();

        let mut events = Vec::new();
        move_to_zone(&mut state, spell, Zone::Stack, &mut events);
        push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
        flush_layers(&mut state);

        assert!(
            perf_counters::snapshot().layers_full_eval >= 1,
            "a Command-zone origin must run a full layers pass on stack arrival"
        );
        // The stack-arrival reach guard for this origin: the spell actually
        // reached the stack under the caster.
        assert_eq!(state.stack.back().unwrap().controller, caster);
    }

    /// ④ HAND — POSITIVE CONTROL, on the mechanism: a hand cast must
    /// increment `layers_full_eval`, proving the instrument fires (paired
    /// with ③'s reliance on the same counter).
    #[test]
    fn hand_origin_runs_a_full_pass() {
        let mut state = new_state();
        let caster = P1;
        let spell = spell_object(&mut state, caster, "Hand Origin Spell", 1, Zone::Hand);
        state.layers_dirty = LayersDirty::Clean;
        perf_counters::reset();

        let mut events = Vec::new();
        move_to_zone(&mut state, spell, Zone::Stack, &mut events);
        push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
        flush_layers(&mut state);

        assert!(
            perf_counters::snapshot().layers_full_eval >= 1,
            "a Hand origin must (and always did) increment layers_full_eval"
        );
    }

    /// HOSTILE (c): a Battlefield-origin move to the stack is already covered
    /// by the pre-existing `from == Zone::Battlefield` arm — U1c must not
    /// double-mark or change its behavior.
    #[test]
    fn battlefield_origin_still_marks_full_via_the_pre_existing_arm() {
        let mut state = new_state();
        let caster = P0;
        let spell = spell_object(
            &mut state,
            caster,
            "Battlefield Origin Spell",
            1,
            Zone::Battlefield,
        );
        state.layers_dirty = LayersDirty::Clean;
        perf_counters::reset();
        let mut events = Vec::new();
        move_to_zone(&mut state, spell, Zone::Stack, &mut events);
        assert!(
            perf_counters::snapshot().layers_full_eval == 0,
            "flush has not run yet; only the mark should be set"
        );
        assert!(matches!(state.layers_dirty, LayersDirty::Full));
    }
}

// ---------------------------------------------------------------------------
// V4 — a LATER control change on a stack object escalates the incremental arm
// ---------------------------------------------------------------------------

/// CR 613.1 + CR 613.1b: `incremental_flush_escalates_when_a_stack_object_is_a_layer_recipient`.
/// REVERT-FAILING: remove the guard conjunct ⇒ incremental arm taken, stack
/// controller stale.
#[test]
fn incremental_flush_escalates_when_a_stack_object_is_a_layer_recipient() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Recipient Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell); // marks Full internally

    let entrant = creature_on_battlefield(&mut state, caster, "Entrant", 3);
    // Simulate that only a battlefield entrant was marked since the last
    // flush — the shape `prepare_incremental_flush`'s cheap arm is designed for.
    state.layers_dirty = LayersDirty::EnteredObjects(BTreeSet::from([entrant]));

    perf_counters::reset();
    flush_layers(&mut state);

    assert!(
        perf_counters::snapshot().layers_full_eval >= 1,
        "a stack recipient must escalate the incremental arm to a full pass"
    );
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(
        stack_object_controller(&state, &entry),
        thief,
        "the escalated full pass must seed the stack object's live controller"
    );
    // REACH GUARD: the entrant itself was processed too (no gratuitous skip).
    assert!(state.battlefield.contains(&entrant));
}

/// SIBLING (V4): same board with NO stack recipient ⇒ the incremental arm is
/// still taken (no gratuitous escalation).
#[test]
fn incremental_flush_does_not_escalate_without_a_stack_recipient() {
    let mut state = new_state();
    let entrant = creature_on_battlefield(&mut state, P0, "Entrant", 1);
    state.layers_dirty = LayersDirty::EnteredObjects(BTreeSet::from([entrant]));
    perf_counters::reset();
    flush_layers(&mut state);
    assert_eq!(
        perf_counters::snapshot().layers_full_eval,
        0,
        "with no stack recipient, the cheap incremental arm must be taken"
    );
}

/// HOSTILE (V4): stack non-empty but no effect names it ⇒
/// `continuous_effect_scan_zones` finds no `Zone::Stack`, arm proceeds.
#[test]
fn incremental_flush_proceeds_with_an_unrelated_stack_spell_present() {
    let mut state = new_state();
    let unrelated = spell_object(&mut state, P1, "Unrelated Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, unrelated, P1, None, CastingVariant::Normal);
    let entrant = creature_on_battlefield(&mut state, P0, "Entrant", 2);
    state.layers_dirty = LayersDirty::EnteredObjects(BTreeSet::from([entrant]));
    perf_counters::reset();
    flush_layers(&mut state);
    assert_eq!(
        perf_counters::snapshot().layers_full_eval,
        0,
        "an unrelated stack spell must not force escalation"
    );
}

// ---------------------------------------------------------------------------
// V5 — a stolen spell RESOLVES FOR ITS NEW CONTROLLER (CR 608.2c)
// ---------------------------------------------------------------------------

/// CR 608.2c + CR 400.7a: `stolen_spell_resolves_for_its_new_controller`.
/// REVERT-FAILING: delete the re-stamp in `resolve_top` and the caster draws.
#[test]
fn stolen_spell_resolves_for_its_new_controller() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    seed_libraries(&mut state, 1000);
    let spell = spell_object(&mut state, caster, "Stolen Draw Spell", 1, Zone::Stack);
    let ability = draw_ability(caster, spell);
    push_spell_entry(
        &mut state,
        spell,
        caster,
        Some(ability),
        CastingVariant::Normal,
    );
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    let caster_hand_before = state
        .players
        .iter()
        .find(|p| p.id == caster)
        .unwrap()
        .hand
        .len();
    let thief_hand_before = state
        .players
        .iter()
        .find(|p| p.id == thief)
        .unwrap()
        .hand
        .len();

    let mut events = Vec::new();
    resolve_top(&mut state, &mut events);

    // REACH GUARD: the spell left the stack.
    assert!(state.stack.is_empty());
    let caster_hand_after = state
        .players
        .iter()
        .find(|p| p.id == caster)
        .unwrap()
        .hand
        .len();
    let thief_hand_after = state
        .players
        .iter()
        .find(|p| p.id == thief)
        .unwrap()
        .hand
        .len();
    assert_eq!(
        thief_hand_after,
        thief_hand_before + 1,
        "the thief (new controller) must draw"
    );
    assert_eq!(
        caster_hand_after, caster_hand_before,
        "the caster (former controller) must not draw"
    );
}

/// SIBLING/NEGATIVE (V5, CR 113.8): an activated ability entry is NOT
/// re-stamped — installing the same effect on an ability entry leaves its
/// controller unchanged, and this is also the positive control for the
/// measured fact that an ability entry has no `state.objects` row.
#[test]
fn activated_ability_entry_is_not_restamped() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    seed_libraries(&mut state, 2000);
    let source = creature_on_battlefield(&mut state, caster, "Ability Source", 1);
    let ability = draw_ability(caster, source);
    state.stack.push_back(StackEntry {
        id: ObjectId(9998),
        source_id: source,
        controller: caster,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    });
    // A "steal" effect naming the ABILITY id — has no object row, so this
    // installs harmlessly and never applies (no recipient to seed).
    install_steal(&mut state, source, thief, ObjectId(9998));
    evaluate_layers(&mut state);

    let caster_hand_before = state
        .players
        .iter()
        .find(|p| p.id == caster)
        .unwrap()
        .hand
        .len();
    let mut events = Vec::new();
    resolve_top(&mut state, &mut events);
    let caster_hand_after = state
        .players
        .iter()
        .find(|p| p.id == caster)
        .unwrap()
        .hand
        .len();
    assert_eq!(
        caster_hand_after,
        caster_hand_before + 1,
        "CR 113.8: an activated ability's controller is fixed at activation and never re-stamped"
    );
}

// ---------------------------------------------------------------------------
// V5b — the RESOLUTION CHOICES of a stolen spell go to its new controller
// ---------------------------------------------------------------------------

mod resolution_choices_route_to_the_live_controller {
    use super::*;

    fn stolen_spell_board(
        card_name: &str,
        card_num: u64,
        keyword: Keyword,
        ability_targets: Vec<TargetRef>,
    ) -> (GameState, ObjectId) {
        let mut state = new_state();
        let caster = P1;
        let thief = P0;
        let spell = spell_object(&mut state, caster, card_name, card_num, Zone::Stack);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .keywords
            .push(keyword.clone());
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .base_keywords
            .push(keyword);
        let ability = ResolvedAbility::new(
            Effect::unimplemented("test", "resolution choice fixture"),
            ability_targets,
            spell,
            caster,
        );
        push_spell_entry(
            &mut state,
            spell,
            caster,
            Some(ability),
            CastingVariant::Normal,
        );
        let source = creature_on_battlefield(&mut state, thief, "Threaten Source", card_num + 100);
        install_steal(&mut state, source, thief, spell);
        evaluate_layers(&mut state);
        // REACH GUARD: the steal landed before resolution.
        let entry = state.stack.back().unwrap().clone();
        assert_eq!(stack_object_controller(&state, &entry), thief);
        (state, spell)
    }

    /// (a) mutate, CR 702.140c: the top/bottom prompt is raised for the THIEF.
    #[test]
    fn mutate_merge_choice_goes_to_the_thief() {
        let (mut state, spell) = stolen_spell_board(
            "Stolen Mutate Spell",
            1,
            Keyword::Mutate(ManaCost::zero()),
            vec![],
        );
        let target = creature_on_battlefield(&mut state, P1, "Mutate Target", 50);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        // Rebuild the ability with the mutate target now that it exists.
        if let StackEntryKind::Spell { ability, .. } = &mut state.stack.back_mut().unwrap().kind {
            *ability = Some(Box::new(ResolvedAbility::new(
                Effect::unimplemented("test", "mutate fixture"),
                vec![TargetRef::Object(target)],
                spell,
                P1,
            )));
        }
        if let StackEntryKind::Spell {
            casting_variant, ..
        } = &mut state.stack.back_mut().unwrap().kind
        {
            *casting_variant = CastingVariant::Mutate;
        }

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        match &state.waiting_for {
            WaitingFor::MutateMergeChoice { player, .. } => assert_eq!(*player, P0),
            other => panic!("expected MutateMergeChoice, got {other:?}"),
        }
        assert_eq!(
            state.active_mutate_merge_frame().unwrap().controller,
            P0,
            "PendingMutateMerge.controller must be the live controller (the thief)"
        );
    }

    /// (b) cipher, CR 702.99a: the encode offer's legal-creature set is drawn
    /// from the THIEF's creatures and the prompt is the thief's.
    #[test]
    fn cipher_encode_offer_uses_the_thiefs_creatures() {
        let (mut state, _spell) =
            stolen_spell_board("Stolen Cipher Spell", 2, Keyword::Cipher, vec![]);
        let thief_creature = creature_on_battlefield(&mut state, P0, "Thief Creature", 60);
        let caster_creature = creature_on_battlefield(&mut state, P1, "Caster Creature", 61);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        match &state.waiting_for {
            WaitingFor::CipherEncodeChoice {
                player, creatures, ..
            } => {
                assert_eq!(*player, P0, "the encode prompt must belong to the thief");
                assert!(
                    creatures.contains(&thief_creature),
                    "the thief's own creature must be offered"
                );
                assert!(
                    !creatures.contains(&caster_creature),
                    "the caster's creature must NOT be offered to the thief"
                );
            }
            other => panic!("expected CipherEncodeChoice, got {other:?}"),
        }
    }

    /// HOSTILE (V5b iii): cipher with the thief controlling NO creature ⇒
    /// `begin_encode_choice` returns false and the card routes normally, no
    /// prompt, no panic.
    #[test]
    fn cipher_with_no_legal_host_routes_normally() {
        let (mut state, spell) =
            stolen_spell_board("Hostless Cipher Spell", 3, Keyword::Cipher, vec![]);
        // `stolen_spell_board` seats a "Threaten Source" creature under the
        // thief to carry the steal effect's source_id; strip it from the
        // battlefield so the thief genuinely controls no legal encode host.
        state
            .battlefield
            .retain(|&id| state.objects[&id].controller != P0);
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert!(
            !matches!(state.waiting_for, WaitingFor::CipherEncodeChoice { .. }),
            "no legal host must not surface an encode prompt"
        );
        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
    }

    /// (c) epic, CR 702.50a: the cast lockout lands on the THIEF.
    #[test]
    fn epic_lockout_lands_on_the_thief() {
        let (mut state, _spell) = stolen_spell_board("Stolen Epic Spell", 4, Keyword::Epic, vec![]);
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert!(
            state.epic_effects.iter().any(|e| e.controller == P0),
            "the Epic lockout (and recurring upkeep copy) must be armed for the thief"
        );
        assert!(
            !state.epic_effects.iter().any(|e| e.controller == P1),
            "the caster must NOT be locked out"
        );
    }

    /// (d) paradigm, CR 702.192a: primes the THIEF, not the caster, and the
    /// linked `ExileLinkKind::ParadigmSource` names the thief too. Its own
    /// extra sibling: a later P1 cast of a same-named spell must still be
    /// primed for P1 (the writer and the `already_primed` reader are the
    /// same value at the same site).
    #[test]
    fn paradigm_primes_the_thief_and_a_later_p1_cast_still_primes_p1() {
        let (mut state, spell) =
            stolen_spell_board("Stolen Paradigm Spell", 5, Keyword::Paradigm, vec![]);
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(
            state
                .paradigm_primed
                .iter()
                .any(|p| p.player == P0 && p.card_name == "Stolen Paradigm Spell"),
            "paradigm_primed must record the THIEF, not the caster"
        );
        assert!(
            !state
                .paradigm_primed
                .iter()
                .any(|p| p.player == P1 && p.card_name == "Stolen Paradigm Spell"),
            "the caster must not be primed"
        );
        assert!(
            state.exile_links.iter().any(|link| matches!(
                link.kind,
                engine::types::game_state::ExileLinkKind::ParadigmSource { player } if player == P0
            )),
            "the ExileLinkKind::ParadigmSource must also name the thief"
        );
        let _ = spell;

        // Sibling: a fresh (unstolen) P1 cast of the same-named card must
        // still prime for P1 — the writer and the already_primed reader
        // agree by construction.
        let second = spell_object(&mut state, P1, "Stolen Paradigm Spell", 6, Zone::Stack);
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .keywords
            .push(Keyword::Paradigm);
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .base_keywords
            .push(Keyword::Paradigm);
        push_spell_entry(&mut state, second, P1, None, CastingVariant::Normal);
        let mut events2 = Vec::new();
        resolve_top(&mut state, &mut events2);
        assert!(
            state
                .paradigm_primed
                .iter()
                .any(|p| p.player == P1 && p.card_name == "Stolen Paradigm Spell"),
            "a same-named spell cast (and controlled) by P1 must still prime for P1"
        );
    }

    /// SIBLING (V5b): the identical board with NO steal ⇒ every prompt/set/
    /// lockout is the caster's, unchanged from BASE_SHA (by-construction
    /// inertness: absent a control effect the latch equals entry.controller).
    #[test]
    fn unstolen_epic_spell_locks_out_the_caster_not_a_thief() {
        let mut state = new_state();
        let caster = P1;
        let spell = spell_object(&mut state, caster, "Unstolen Epic Spell", 7, Zone::Stack);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .keywords
            .push(Keyword::Epic);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .base_keywords
            .push(Keyword::Epic);
        let ability = ResolvedAbility::new(
            Effect::unimplemented("test", "epic fixture"),
            vec![],
            spell,
            caster,
        );
        push_spell_entry(
            &mut state,
            spell,
            caster,
            Some(ability),
            CastingVariant::Normal,
        );
        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert!(state.epic_effects.iter().any(|e| e.controller == caster));
        assert!(!state.epic_effects.iter().any(|e| e.controller == P0));
    }

    /// HOSTILE (V5b ii): an ABILITY entry on the same board ⇒ no
    /// `state.objects` row, latch falls back to `entry.controller` per CR
    /// 113.8, so no ability's controller is re-stamped (covered structurally
    /// by `activated_ability_entry_is_not_restamped` above; this pins the
    /// SAME invariant on a triggered ability entry specifically).
    #[test]
    fn triggered_ability_entry_is_not_restamped_after_a_steal_on_its_id() {
        let mut state = new_state();
        let source = creature_on_battlefield(&mut state, P1, "Trigger Source", 1);
        let ability_id = ObjectId(9997);
        let ability = draw_ability(P1, source);
        state.stack.push_back(StackEntry {
            id: ability_id,
            source_id: source,
            controller: P1,
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Trigger Source".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });
        install_steal(&mut state, source, P0, ability_id);
        evaluate_layers(&mut state);
        let entry = state.stack.back().unwrap().clone();
        assert_eq!(
            stack_object_controller(&state, &entry),
            P1,
            "an ability entry has no object row; the accessor falls back to entry.controller"
        );
    }
}

// ---------------------------------------------------------------------------
// V5c — the by-default sites still answer the CR 112.2 caster
// ---------------------------------------------------------------------------

/// `by_default_stack_entry_controller_sites_still_answer_the_caster` — mutate's
/// target recheck (`FilterContext::from_source_with_controller(entry.id,
/// entry.controller)`) is a by-default (`entry.controller`) site that P1.5
/// deliberately does NOT route to `live_controller`. REVERT-FAILING: route
/// this site to `live_controller` and BOTH sub-cases below flip — a target
/// owned by the THIEF would become legal, and a target owned by the CASTER
/// would become illegal, once the spell is stolen.
///
/// (The recheck's own approximation of CR 702.140a's true owner axis —
/// `entry.controller` rather than `state.objects[&entry.id].owner` — is a
/// separate, pre-existing `KNOWN LIMITATION` this row does not exercise; it
/// is annotated at the site itself, not fixed by this run.)
#[test]
fn mutate_recheck_stays_anchored_to_entry_controller_after_a_steal() {
    let caster = P1;
    let thief = P0;

    // Sub-case A: a target owned by the CASTER remains LEGAL after the steal
    // (the recheck still reads `entry.controller`, unaffected by the thief's
    // live control).
    {
        let mut state = new_state();
        let spell = spell_object(&mut state, caster, "Stolen Mutate Spell A", 1, Zone::Stack);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let target = creature_on_battlefield(&mut state, caster, "Caster-Owned Target", 2);
        let ability = ResolvedAbility::new(
            Effect::unimplemented("test", "mutate recheck fixture A"),
            vec![TargetRef::Object(target)],
            spell,
            caster,
        );
        push_spell_entry(
            &mut state,
            spell,
            caster,
            Some(ability),
            CastingVariant::Mutate,
        );
        let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 3);
        install_steal(&mut state, source, thief, spell);
        evaluate_layers(&mut state);
        // REACH GUARD: the steal landed.
        let entry = state.stack.back().unwrap().clone();
        assert_eq!(stack_object_controller(&state, &entry), thief);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert!(
            matches!(state.waiting_for, WaitingFor::MutateMergeChoice { .. }),
            "a caster-owned target must remain LEGAL after the steal (recheck still \
             reads entry.controller, not live_controller)"
        );
    }

    // Sub-case B: a target owned by the THIEF remains ILLEGAL after the
    // steal — if this site were wrongly routed to `live_controller`, the
    // thief's own creature would become a legal target.
    {
        let mut state = new_state();
        let spell = spell_object(&mut state, caster, "Stolen Mutate Spell B", 1, Zone::Stack);
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let target = creature_on_battlefield(&mut state, thief, "Thief-Owned Target", 2);
        let ability = ResolvedAbility::new(
            Effect::unimplemented("test", "mutate recheck fixture B"),
            vec![TargetRef::Object(target)],
            spell,
            caster,
        );
        push_spell_entry(
            &mut state,
            spell,
            caster,
            Some(ability),
            CastingVariant::Mutate,
        );
        let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 3);
        install_steal(&mut state, source, thief, spell);
        evaluate_layers(&mut state);
        let entry = state.stack.back().unwrap().clone();
        assert_eq!(stack_object_controller(&state, &entry), thief);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);
        assert!(
            !matches!(state.waiting_for, WaitingFor::MutateMergeChoice { .. }),
            "a thief-owned target must remain ILLEGAL after the steal — the recheck \
             does not follow live_controller"
        );
        // REACH GUARD: the spell resolved (as a plain creature), not stuck/errored.
        assert_eq!(state.objects[&spell].zone, Zone::Battlefield);
    }
}

/// V5 — CR 109.4 + CR 108.4a + CR 608.2h: `stack_exit_resets_the_controller_and_snapshots_the_thief_into_lki`.
///
/// REPLACES the prior `stack_exit_residual_a_rebound_spell_carries_the_thiefs_controller_into_exile`
/// row, which drove `resolve_top` directly against a hand-built Rebound spell.
/// That direct call skips the mid-resolution layer flush the FULL pipeline
/// performs (`stack::resolve_top`'s own re-seed from `resolving_stack_entry.controller`),
/// which is why it manufactured a residual a real rebound spell never shows
/// (MEASURED: Probe D2/F — the full pipeline already reads `controller ==
/// caster` for a resolving/rebounding steal, unpatched). The REAL reproducing
/// class is the NON-RESOLVING stack exit — a stolen spell countered and
/// exiled instead of hitting the graveyard (Dissipate: "Counter target spell.
/// If that spell is countered this way, exile it instead of putting it into
/// its owner's graveyard."), which never runs `resolve_top`'s mid-resolution
/// flush at all (MEASURED: Probe G — `controller` stayed the THIEF at
/// baseline). This row drives that exact seam: `effects::counter::resolve`
/// removes the stack entry via `stack::remove_nonresolving_stack_entry_at`
/// and then routes the Stack→Exile move through
/// `zone_pipeline::move_object` → `zones::move_to_zone` →
/// `zones::apply_zone_exit_cleanup` — the same `move_to_zone` primitive
/// called directly here, consistent with this file's own stated convention
/// (see the file header) of exercising the exact zone-transition production
/// function rather than reconstructing the full alternative-cost cast UI.
///
/// REVERT-FAILING: delete either half of U2 (the LKI-capture widening or the
/// CR 109.4 reset) and this fails — the reset alone reds this row's LKI
/// assertion (LKI never captured for a stack exit), and the reset without the
/// LKI capture leaves `obj.controller` correct but `lki_cache` empty.
#[test]
fn stack_exit_resets_the_controller_and_snapshots_the_thief_into_lki() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Stolen Countered Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    // REACH GUARD: the steal landed and the spell is still mid-flight on the
    // stack before the (simulated) Dissipate-class counter-and-exile.
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(stack_object_controller(&state, &entry), thief);
    assert_eq!(state.objects[&spell].zone, Zone::Stack);

    // CR 701.6a: the removal from the stack IS the counter — mirrors
    // `effects::counter::resolve`'s own `remove_nonresolving_stack_entry_at`
    // call (not reachable from an external integration test; `retain`
    // reproduces the same "gone from state.stack" postcondition for this
    // row's purposes, since the claim under test is entirely inside the
    // subsequent zone move, not the stack-removal bookkeeping itself).
    state.stack.retain(|e| e.id != spell);

    // CR 614.1a (Dissipate's exile-instead rider): the countered spell's
    // destination move, driven through the exact zone-exit authority
    // (`zones::move_to_zone` → `zones::apply_zone_exit_cleanup`) that
    // `zone_pipeline::move_object` calls in production.
    let mut events = Vec::new();
    move_to_zone(&mut state, spell, Zone::Exile, &mut events);

    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "REACH GUARD: the countered spell actually reached Exile"
    );
    assert_eq!(
        state.objects[&spell].controller, caster,
        "CR 109.4 + CR 108.4a: an object arriving in a zone with no controller carries the \
         owner fallback, not the thief's stale layer-2 control change"
    );
    assert_eq!(
        state
            .lki_cache
            .get(&spell)
            .expect("the stack exit must capture LKI")
            .controller,
        thief,
        "CR 608.2h: the at-exit (thief) controller must survive in LKI for a later look-back"
    );
}

// ---------------------------------------------------------------------------
// V6 — "counter target spell an opponent controls" follows the live controller
// ---------------------------------------------------------------------------

/// `counter_target_spell_an_opponent_controls_follows_the_live_controller`.
/// REVERT-FAILING: delete the `obj.controller = base` seed line and the
/// stolen spell stays targetable by its thief (STICKY origin-zone data).
#[test]
fn counter_target_spell_an_opponent_controls_follows_the_live_controller() {
    let mut state = new_state();
    let victim_caster = P1;
    let thief = P0;
    let counter_caster = P0; // the thief's own counterspell, post-steal.
    let spell = spell_object(&mut state, victim_caster, "Victim Spell", 1, Zone::Stack);
    push_spell_entry(
        &mut state,
        spell,
        victim_caster,
        None,
        CastingVariant::Normal,
    );

    let opponent_owned_stack_spell = TargetFilter::And {
        filters: vec![
            TargetFilter::StackSpell,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        ],
    };
    let counter_source = ObjectId(8001);

    // BEFORE the steal: legal for P0 (an opponent, from P0's perspective, controls it).
    let before = find_legal_targets(
        &state,
        &opponent_owned_stack_spell,
        counter_caster,
        counter_source,
    );
    assert!(
        before.contains(&TargetRef::Object(spell)),
        "REACH GUARD: the counterspell could target the victim spell before the steal"
    );

    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    let entry = state.stack.back().unwrap().clone();
    assert_eq!(stack_object_controller(&state, &entry), thief);

    let after = find_legal_targets(
        &state,
        &opponent_owned_stack_spell,
        counter_caster,
        counter_source,
    );
    assert!(
        !after.contains(&TargetRef::Object(spell)),
        "after the steal, the spell is controlled by the (former opponent's) thief itself \
         — no longer \"an opponent controls\" it"
    );
}

/// SIBLING (V6): `ControllerRef::You` exercised on the same board (legality
/// flips the OTHER way).
#[test]
fn counter_target_spell_you_control_flips_the_other_way() {
    let mut state = new_state();
    let victim_caster = P1;
    let thief = P0;
    let counter_caster = P0;
    let spell = spell_object(&mut state, victim_caster, "Victim Spell", 1, Zone::Stack);
    push_spell_entry(
        &mut state,
        spell,
        victim_caster,
        None,
        CastingVariant::Normal,
    );

    let you_controlled_stack_spell = TargetFilter::And {
        filters: vec![
            TargetFilter::StackSpell,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        ],
    };
    let counter_source = ObjectId(8002);
    let before = find_legal_targets(
        &state,
        &you_controlled_stack_spell,
        counter_caster,
        counter_source,
    );
    assert!(!before.contains(&TargetRef::Object(spell)));

    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    let after = find_legal_targets(
        &state,
        &you_controlled_stack_spell,
        counter_caster,
        counter_source,
    );
    assert!(
        after.contains(&TargetRef::Object(spell)),
        "after the steal, the spell is now the thief's own — a legal `You` target"
    );
}

// ---------------------------------------------------------------------------
// V7 — TriggeringSpellController follows control; TriggeringSpellOwner does not
// ---------------------------------------------------------------------------

#[test]
fn that_spells_controller_follows_a_control_change() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "That Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    state.current_trigger_event = Some(GameEvent::SpellCast {
        card_id: state.objects[&spell].card_id,
        controller: caster,
        object_id: spell,
        cast_mana_value: None,
    });
    let ability = ResolvedAbility::new(Effect::unimplemented("test", "V7"), vec![], spell, caster);

    let controller_target =
        resolved_targets(&ability, &TargetFilter::TriggeringSpellController, &state);
    assert_eq!(
        controller_target,
        vec![TargetRef::Player(thief)],
        "TriggeringSpellController must follow the live control change"
    );
}

#[test]
fn that_spells_owner_does_not_follow_a_control_change() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "That Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    state.current_trigger_event = Some(GameEvent::SpellCast {
        card_id: state.objects[&spell].card_id,
        controller: caster,
        object_id: spell,
        cast_mana_value: None,
    });
    let ability = ResolvedAbility::new(Effect::unimplemented("test", "V7"), vec![], spell, caster);
    let owner_target = resolved_targets(&ability, &TargetFilter::TriggeringSpellOwner, &state);
    assert_eq!(
        owner_target,
        vec![TargetRef::Player(caster)],
        "TriggeringSpellOwner must remain the owner, unaffected by the steal"
    );
}

/// HOSTILE (V7): Gonti-class spell (owner != caster) with NO exchange ⇒
/// controller == caster, owner == owner — the two must DIFFER.
#[test]
fn triggering_spell_controller_and_owner_diverge_for_a_gonti_class_cast() {
    let mut state = new_state();
    let owner = P1;
    let caster = P0;
    let spell = spell_object(&mut state, owner, "Gonti Class That Spell", 1, Zone::Exile);
    state.layers_dirty = LayersDirty::Clean;
    let mut events = Vec::new();
    move_to_zone(&mut state, spell, Zone::Stack, &mut events);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    flush_layers(&mut state);

    state.current_trigger_event = Some(GameEvent::SpellCast {
        card_id: state.objects[&spell].card_id,
        controller: caster,
        object_id: spell,
        cast_mana_value: None,
    });
    let ability = ResolvedAbility::new(Effect::unimplemented("test", "V7"), vec![], spell, caster);
    let controller_target =
        resolved_targets(&ability, &TargetFilter::TriggeringSpellController, &state);
    let owner_target = resolved_targets(&ability, &TargetFilter::TriggeringSpellOwner, &state);
    assert_eq!(controller_target, vec![TargetRef::Player(caster)]);
    assert_eq!(owner_target, vec![TargetRef::Player(owner)]);
    assert_ne!(controller_target, owner_target);
}

// ---------------------------------------------------------------------------
// V8 — CR 800.4a: the sweep's predicate is the post-step-1 controller
// ---------------------------------------------------------------------------

#[test]
fn cr800_4a_thief_leaving_reverts_the_stolen_spell_and_leaves_it_on_the_stack() {
    let mut state = new_state();
    let caster = P1; // owner + caster
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Stolen Spell CR800.4a", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    // REACH GUARD: the steal landed, spell is on the stack.
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(stack_object_controller(&state, &entry), thief);
    assert_eq!(state.stack.len(), 1);

    let mut events = Vec::new();
    eliminate_player(&mut state, thief, &mut events);

    // Leg 2 (REVERT-FAILING): the spell reverts to its by-default caster and
    // stays on the stack.
    assert!(
        state.stack.iter().any(|e| e.id == spell),
        "the spell must remain on the stack after the THIEF leaves"
    );
    let entry_after = state.stack.iter().find(|e| e.id == spell).unwrap().clone();
    assert_eq!(
        stack_object_controller(&state, &entry_after),
        caster,
        "the reverted spell's live controller must be the caster once the thief's control effect ends"
    );
}

/// V3b — CR 800.4a step 4: the spell reaches `Zone::Exile`, not merely off
/// the stack. Previously this row (under its same name) pinned a
/// reviewer-adjudicated DOWNGRADE, where the spell left the stack but was
/// stranded (`obj.zone` stayed `Stack`, never exiled) because the sweep ran
/// INSIDE `do_eliminate`, before `end_control_effects_for_leaving_players`.
/// Relocating the sweep past that step (U1/U1b) closes the downgrade: the
/// spell is now correctly exiled per CR 800.4a's own fourth step, so this
/// row asserts the CORRECT disposition rather than pinning the prior gap.
#[test]
fn cr800_4a_caster_leaving_removes_the_stolen_spell() {
    let mut state = new_state();
    let caster = P1; // owner + caster
    let thief = P0;
    let spell = spell_object(
        &mut state,
        caster,
        "Stolen Spell CR800.4a (b)",
        1,
        Zone::Stack,
    );
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    let entry = state.stack.back().unwrap().clone();
    assert_eq!(stack_object_controller(&state, &entry), thief);

    let mut events = Vec::new();
    eliminate_player(&mut state, caster, &mut events);

    assert!(
        !state.stack.iter().any(|e| e.id == spell),
        "the spell must leave the stack when its CR 112.2 by-default caster leaves"
    );
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "CR 800.4a step 4: a card-represented stack object still controlled by a leaver \
         is exiled, not merely un-stacked"
    );
    assert!(
        state.exile.contains(&spell),
        "the exiled spell must also be present in state.exile"
    );
}

/// HOSTILE (V8, CR 800.4a + CR 800.4c): the THIEF leaving reverts a stolen
/// PERMANENT to its by-default controller (the caster, who has NOT left) —
/// it is NOT exiled, because CR 800.4c's exile clause additionally requires
/// the BY-DEFAULT controller to have left, which is not this fixture's shape.
/// This is the direct round-1 B1 regression pin, exercised on a permanent
/// (not a stack object) to confirm P1.1's stack-scoped seed does not disturb
/// the pre-existing battlefield `end_control_effects_for_leaving_players`
/// behavior.
#[test]
fn cr800_4a_thief_leaving_reverts_a_stolen_permanent_to_its_by_default_controller() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let permanent = creature_on_battlefield(&mut state, caster, "Stolen Permanent", 1);
    state.objects.get_mut(&permanent).unwrap().base_controller = Some(caster);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, permanent);
    evaluate_layers(&mut state);
    assert_eq!(state.objects[&permanent].controller, thief);

    let mut events = Vec::new();
    eliminate_player(&mut state, thief, &mut events);
    assert_eq!(
        state.objects[&permanent].zone,
        Zone::Battlefield,
        "the by-default controller (caster) has not left, so CR 800.4c's exile clause \
         does not fire — the permanent reverts to the caster instead"
    );
    assert_eq!(
        state.objects[&permanent].controller, caster,
        "CR 800.4a: ending the thief's control effect reverts the permanent to its \
         by-default controller"
    );
}

// ---------------------------------------------------------------------------
// PR #8332 round 1, U1/U1b — plan-round1 V1/V3/V4: the relocated CR 800.4a
// stack sweep (past control-effect end) and its CR 733 journal attribution.
// (Distinct from this file's own pre-existing "V8" CR 800.4a section above,
// which predates this round's plan and its own V-numbering.)
// ---------------------------------------------------------------------------

/// plan-round1 V1 — a survivor-owned spell controlled by a SURVIVOR is not
/// removed when its eliminated CASTER leaves. Probe A's discriminator:
/// `entry.controller == caster`, `obj.owner == third_player`, `obj.controller
/// == thief` — all three players distinct, the only shape that discriminates
/// the relocated sweep's live-controller predicate
/// (`stack::stack_object_controller`) from the old `entry.controller`
/// predicate.
/// REVERT-FAILING: revert U1 (key the sweep back on `entry.controller`, or
/// leave it running before `end_control_effects_for_leaving_players`) and the
/// spell is wrongly removed when the CASTER leaves, even though its
/// surviving THIEF still controls it.
#[test]
fn cr800_4a_caster_leaving_leaves_a_stolen_spell_under_its_surviving_thief() {
    let mut runner = GameScenario::new_n_player(3, 7).build();
    let state = runner.state_mut();
    let owner = PlayerId(2); // survivor, owns the card (Gonti class)
    let caster = P0; // eliminated
    let thief = P1; // survivor, steals the spell
    let spell = spell_object(state, owner, "Gonti Class Stolen Spell", 1, Zone::Stack);
    push_spell_entry(state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(state, thief, "Threaten Source", 2);
    install_steal(state, source, thief, spell);
    evaluate_layers(state);

    // REACH GUARD: all three players distinct before elimination — the only
    // shape that discriminates this row.
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(entry.controller, caster);
    assert_eq!(state.objects[&spell].owner, owner);
    assert_eq!(stack_object_controller(state, &entry), thief);

    let mut events = Vec::new();
    eliminate_player(state, caster, &mut events);

    assert!(
        state.stack.iter().any(|e| e.id == spell),
        "the spell must remain on the stack — its surviving thief still controls it"
    );
    let entry_after = state.stack.iter().find(|e| e.id == spell).unwrap().clone();
    assert_eq!(
        stack_object_controller(state, &entry_after),
        thief,
        "the surviving thief's control effect did not end (only the CASTER left), so the \
         spell keeps resolving for them"
    );
}

/// plan-round1 V3 — CR 800.4a step 4: a survivor-owned spell STILL controlled
/// by the leaver (no theft) is EXILED, not merely un-stacked. Probe I's
/// shape: same board as V1 but no steal — the leaving CASTER is also the
/// spell's by-default controller when they leave.
/// REVERT-FAILING: before U1b's relocation, the sweep ran inside
/// `do_eliminate` and had no exile step at all — the entry left the stack but
/// `obj.zone` stayed `Stack`, never reaching Exile (measured Probe I/C).
#[test]
fn cr800_4a_a_leavers_spell_on_a_survivors_card_is_exiled() {
    let mut runner = GameScenario::new_n_player(3, 8).build();
    let state = runner.state_mut();
    let owner = PlayerId(2); // survivor, owns the card
    let caster = P0; // eliminated; also the by-default (and live) controller
    let spell = spell_object(
        state,
        owner,
        "Gonti Class Uncontested Spell",
        1,
        Zone::Stack,
    );
    push_spell_entry(state, spell, caster, None, CastingVariant::Normal);
    evaluate_layers(state);

    // REACH GUARD: entry present, owner is the survivor, no theft in play.
    let entry = state.stack.back().unwrap().clone();
    assert_eq!(entry.controller, caster);
    assert_eq!(state.objects[&spell].owner, owner);
    assert_eq!(stack_object_controller(state, &entry), caster);

    let mut events = Vec::new();
    eliminate_player(state, caster, &mut events);

    assert!(
        !state.stack.iter().any(|e| e.id == spell),
        "the spell must leave the stack when its by-default caster leaves"
    );
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "CR 800.4a step 4: a card-represented stack object still controlled by a leaver \
         is exiled, not merely un-stacked"
    );
    assert_eq!(
        state.objects[&spell].controller, owner,
        "CR 109.4 + CR 108.4a: the exile exit resets the controller to the owner fallback"
    );
    assert!(state.exile.contains(&spell));
}

/// plan-round1 V4 — the relocated sweep preserves per-player CR 733 journal
/// attribution: two players eliminated SIMULTANEOUSLY, each holding a stack
/// object, must journal their removals under DIFFERENT leave nodes, not one
/// shared node.
/// REVERT-FAILING: if the relocated sweep reused a single
/// `active_rules_execution_node` across the whole `leave_nodes` batch instead
/// of re-installing each player's own node per iteration, both removals would
/// carry the SAME `cause`.
#[test]
fn cr800_4a_simultaneous_leavers_journal_their_own_stack_removals() {
    let mut state = new_state();
    let p0_spell = spell_object(&mut state, P0, "P0 Stack Object", 1, Zone::Stack);
    push_spell_entry(&mut state, p0_spell, P0, None, CastingVariant::Normal);
    let p1_spell = spell_object(&mut state, P1, "P1 Stack Object", 2, Zone::Stack);
    push_spell_entry(&mut state, p1_spell, P1, None, CastingVariant::Normal);
    evaluate_layers(&mut state);

    // REACH GUARD: both entries present before the simultaneous elimination.
    assert_eq!(state.stack.len(), 2);

    let mut events = Vec::new();
    engine::game::elimination::eliminate_players_simultaneously(&mut state, &[P0, P1], &mut events);

    assert!(
        state.stack.is_empty(),
        "both players' own stack objects must be removed"
    );

    let removal_causes: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|e| match &e.command {
            Some(engine::types::resolved_commands::ResolvedRulesCommand::StackRemoval(cmd)) => {
                Some(cmd.cause)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        removal_causes.len(),
        2,
        "each player's own stack removal must be journaled"
    );
    assert_ne!(
        removal_causes[0], removal_causes[1],
        "CR 733: each removal must be journaled under its OWN player's leave node, not a \
         shared one"
    );
}

// ---------------------------------------------------------------------------
// PR #8332 round 1, U3 — plan-round1 V8/V8b: CR 603.10d + CR 603.3a lose-
// control trigger controller derivation
// ---------------------------------------------------------------------------

/// Verbatim from `client/public/card-data.json`.
const KHARN_TEXT_R1: &str = "Berzerker — Khârn the Betrayer attacks or blocks each combat if \
    able.\nSigil of Corruption — When you lose control of Khârn the Betrayer, draw two \
    cards.\nThe Betrayer — If damage would be dealt to Khârn the Betrayer, prevent that damage \
    and an opponent of your choice gains control of it.";

/// Verbatim from `client/public/card-data.json`.
const ACT_OF_TREASON_TEXT: &str = "Gain control of target creature until end of turn.";

/// plan-round1 V8 — Probe K's shape: NO stack subject, NO exchange — Khârn
/// sits on P0's battlefield and P1 casts Act of Treason on it. CR 603.10d:
/// "when you lose control of ~" looks back in time; CR 603.3a / CR 113.8: a
/// triggered ability is controlled by whoever controlled its source AT THE
/// TIME IT TRIGGERED, i.e. the PRE-change controller — P0, who just lost
/// control — not P1, the gainer.
/// REVERT-FAILING (measured, Probe K): before U3 the engine gives both cards
/// to P1 (the gainer), zero to P0. This defect is pre-existing and has
/// nothing to do with any stack object or exchange — reproducible with a
/// plain battlefield steal.
#[test]
fn cr603_10d_lose_control_trigger_is_controlled_by_the_player_who_lost_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B", "P0 Draw C"]);
    scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B", "P1 Draw C"]);
    let kharn = scenario
        .add_creature_from_oracle(P0, "Khârn the Betrayer", 4, 4, KHARN_TEXT_R1)
        .id();
    let act_of_treason = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Treason", false, ACT_OF_TREASON_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let outcome = runner
        .cast(act_of_treason)
        .target_objects(&[kharn])
        .resolve();

    // REACH GUARD: the steal actually landed.
    assert_eq!(
        outcome.state().objects.get(&kharn).unwrap().controller,
        P1,
        "REACH GUARD: Act of Treason must have taken control of Khârn"
    );
    assert_eq!(
        outcome.hand_drawn(P0),
        2,
        "CR 603.10d: the player who LOST control (P0) controls the trigger and must draw"
    );
    assert_eq!(
        outcome.hand_drawn(P1),
        0,
        "the gainer (P1) must not draw — reversing the recipient is the pre-existing bug"
    );
}

/// plan-round1 V8b — the delayed/`SpecificObject` shape (Stolen Uniform's
/// reflexive "when you lose control of the enchanted creature this turn,
/// draw a card") is sourced from a DIFFERENT object than the one whose
/// control changed (`source_id != object_id`). Its controller comes from a
/// wholly separate authority — `DelayedTrigger.controller`, bound once at
/// installation and read directly by `triggers::delayed_trigger_to_context`
/// — never from `collect_matching_triggers_inner`'s CR 603.10d override
/// above (which is gated on `trig_def.mode == ChangesController && *object_id
/// == obj_id`, false here by construction). This row pins that the class
/// still resolves for the correct player (P0, the temp holder who created
/// the delayed trigger) after U3, confirming the two authorities do not
/// collide — the "must NOT be re-keyed" half of V8b.
#[test]
fn cr603_10d_delayed_specific_object_shape_still_attributes_to_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B"]);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B"]);
    let source = scenario
        .add_creature(P0, "Stolen Uniform Source", 0, 0)
        .id();
    let sword = scenario.add_creature(P1, "Sword", 2, 2).id();
    let mut runner = scenario.build();

    {
        let obj = runner.state_mut().objects.get_mut(&sword).unwrap();
        obj.card_types = engine::types::card_type::CardType::default();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
    }
    {
        let st = runner.state_mut();
        st.objects.get_mut(&source).unwrap().zone = Zone::Graveyard;
        st.battlefield.retain(|&x| x != source);
        st.players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .graveyard
            .push_back(source);
    }

    // P0 gains control of the Sword until EOT (the "Stolen Uniform" temp control).
    runner.state_mut().add_transient_continuous_effect(
        source,
        P0,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: sword },
        vec![ContinuousModification::ChangeController],
        None,
    );
    flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&sword].controller,
        P0,
        "REACH GUARD: P0 temporarily controls the Sword"
    );

    // The delayed "when you lose control of the enchanted creature this
    // turn, draw a card" reflexive, sourced from the graveyard Stolen-
    // Uniform-class card — source_id (source) != object_id (sword).
    let draw_def = engine::types::ability::AbilityDefinition::new(
        engine::types::ability::AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let mut ability = engine::game::ability_utils::build_resolved_from_def(&draw_def, source, P0);
    ability.set_trigger_source_recursive(engine::game::triggers::trigger_source_context_for_latch(
        runner.state(),
        runner.state().objects.get(&source).unwrap(),
    ));
    let mut trig = engine::types::ability::TriggerDefinition::new(
        engine::types::triggers::TriggerMode::ChangesController,
    );
    trig.valid_card = Some(TargetFilter::SpecificObject { id: sword });
    trig.execute = None;
    runner
        .state_mut()
        .delayed_triggers
        .push(engine::types::game_state::DelayedTrigger::new(
            engine::types::ability::DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(trig),
                or_trigger: None,
                lifetime: engine::types::ability::DelayedTriggerLifetime::ThisTurn,
            },
            Box::new(ability),
            P0,
            source,
            true,
        ));

    let p0_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len();
    let p1_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .hand
        .len();
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&sword].controller,
        P1,
        "REACH GUARD: control reverts to owner P1 at cleanup"
    );
    let p0_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len();
    let p1_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .hand
        .len();
    assert_eq!(
        p0_after,
        p0_before + 1,
        "the delayed trigger's OWN authority (DelayedTrigger.controller = P0, bound at \
         install) must attribute the draw to P0, unaffected by U3's override"
    );
    assert_eq!(
        p1_after, p1_before,
        "P1 (the object's owner, regaining control) must not draw"
    );
}

// ---------------------------------------------------------------------------
// PR #8332 round 1, LOW regression — plan-round1 V9: the CR 601.2a
// announcement window falls back to StackEntry.controller
// ---------------------------------------------------------------------------

/// plan-round1 V9 — during the CR 601.2a announcement window (before a
/// paused target-selection cast finalizes), `derive_views`'s stack-entry
/// controller projection must fall back to `StackEntry.controller` rather
/// than reading the object's own (still-in-origin-zone) controller. Probe J's
/// shape: a P2-owned card in exile carrying `PlayFromExile{granted_to: P0}`,
/// P0 casts it, and the fixture supplies >= 2 legal targets so the cast
/// genuinely pauses at `TargetSelection` (mandatory reach guard — with only
/// one legal target the cast auto-selects and the window is never entered,
/// measured Probe J v1).
#[test]
fn announcement_window_display_uses_the_stack_entry_controller() {
    let mut runner = GameScenario::new_n_player(3, 9).build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let owner = PlayerId(2);
    let caster = P0;
    let (spell, target_a, target_b, card_id) = {
        let state = runner.state_mut();
        let spell = spell_object(state, owner, "Two-Target Exile Grant Spell", 1, Zone::Exile);
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            // A real targeted ability, so the cast genuinely pauses at
            // TargetSelection instead of resolving straight through — with
            // >= 2 legal creature targets on the board (mandatory reach
            // guard: with exactly one legal target the cast auto-selects and
            // the announcement window is never entered, measured Probe J v1).
            let pump = engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Spell,
                Effect::Pump {
                    power: engine::types::ability::PtValue::Fixed(1),
                    toughness: engine::types::ability::PtValue::Fixed(1),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            );
            std::sync::Arc::make_mut(&mut obj.abilities).push(pump.clone());
            std::sync::Arc::make_mut(&mut obj.base_abilities).push(pump);
            obj.casting_permissions.push(
                engine::types::ability::CastingPermission::PlayFromExile {
                    provenance: engine::types::ability::PlayFromExileProvenance::Impulse,
                    duration: Duration::UntilEndOfTurn,
                    granted_to: caster,
                    mode: engine::types::ability::CardPlayMode::Play,
                    frequency: engine::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    invalidation: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    alt_ability_cost: None,
                    land_enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                },
            );
            obj.mana_cost = ManaCost::zero();
        }
        let target_a = creature_on_battlefield(state, owner, "Legal Target A", 2);
        let target_b = creature_on_battlefield(state, owner, "Legal Target B", 3);
        let card_id = state.objects[&spell].card_id;
        (spell, target_a, target_b, card_id)
    };

    let action = engine::types::actions::GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    };
    engine::game::engine::apply(runner.state_mut(), caster, action)
        .expect("cast announcement must succeed");

    // REACH GUARD: the cast genuinely paused at TargetSelection with >= 2
    // legal targets, the spell still physically in Exile, and the stack
    // carrying exactly the one announced entry.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "REACH GUARD: the cast must pause at TargetSelection (got {:?})",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&spell].controller,
        owner,
        "REACH GUARD: the object itself, still in its origin zone, carries no meaningful \
         controller (CR 109.4) — it reads the OWNER, the wrong answer the guard exists to avoid"
    );
    let _ = (target_a, target_b);

    let views = engine::game::derived_views::derive_views(runner.state(), Some(P1));
    let entry_id = runner.state().stack.back().unwrap().id;
    assert_eq!(
        views.stack_entry_details[&entry_id].controller, caster,
        "the announcement-window display must fall back to StackEntry.controller (the caster), \
         not the exiled object's own controller"
    );
}

// ---------------------------------------------------------------------------
// V9 — CR 700.13 Crime detection sees the live controller
// ---------------------------------------------------------------------------

/// `targeting_a_stolen_spell_commits_a_crime_against_its_new_controller`.
/// Driven through the REAL cast pipeline (`GameRunner`/`GameScenario`) because
/// `casting::targets_commit_crime` is `pub(crate)` and only reachable that way
/// from an integration test.
#[test]
fn targeting_a_stolen_spell_does_not_commit_a_crime_against_its_new_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_spell_to_hand(P1, "Victim Spell", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ))
        .id();
    let counter_filter = TargetFilter::StackSpell;
    let probe_a = scenario
        .add_spell_to_hand(P0, "Probe Counter A", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Counter {
                target: counter_filter.clone(),
                source_rider: None,
                countered_spell_zone: None,
            },
        ))
        .id();
    let probe_b = scenario
        .add_spell_to_hand(P0, "Probe Counter B", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Counter {
                target: counter_filter,
                source_rider: None,
                countered_spell_zone: None,
            },
        ))
        .id();
    let thief_source = scenario.add_creature(P0, "Threaten Source", 2, 2).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let mut victim_commit = runner.cast(victim).commit();
    let victim_stack_id = victim_commit.state().stack.back().unwrap().id;

    // SIBLING / REACH GUARD: pre-steal, targeting an opponent-controlled
    // stack spell IS a crime.
    {
        let state = victim_commit.state_mut();
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    victim_commit
        .cast(probe_a)
        .target_object(victim_stack_id)
        .commit();
    let crimes_before = victim_commit
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .crimes_committed_this_turn;
    assert_eq!(
        crimes_before, 1,
        "REACH GUARD: targeting an opponent-controlled stack spell is a crime pre-steal"
    );

    // Now steal the victim spell.
    {
        let state = victim_commit.state_mut();
        state.add_transient_continuous_effect(
            thief_source,
            P0,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject {
                id: victim_stack_id,
            },
            vec![ContinuousModification::ChangeController],
            None,
        );
        evaluate_layers(state);
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // PRIMARY CLAIM (REVERT-FAILING): targeting the now-self-controlled spell
    // must NOT commit a second crime.
    victim_commit
        .cast(probe_b)
        .target_object(victim_stack_id)
        .commit();
    let crimes_after = victim_commit
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .crimes_committed_this_turn;
    assert_eq!(
        crimes_after, 1,
        "targeting a spell the caster now controls (post-steal) must NOT be a crime"
    );
}

// ---------------------------------------------------------------------------
// V10 — "its controller" on a stack target (CR 109.4)
// ---------------------------------------------------------------------------

#[test]
fn parent_target_controller_of_a_stolen_spell_is_the_thief() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Parent Target Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    let ability = ResolvedAbility::new(
        Effect::unimplemented("test", "V10"),
        vec![TargetRef::Object(spell)],
        spell,
        caster,
    );
    assert_eq!(parent_target_controller(&ability, &state), Some(thief));
}

/// HOSTILE (V10): the rung matches by `entry.source_id == id` too — an
/// ability entry matched by `source_id`, where the accessor must fall back to
/// `entry.controller` per CR 113.8 (no object row for the ability id itself).
#[test]
fn parent_target_controller_matches_by_source_id_for_an_ability_entry() {
    let mut state = new_state();
    let source = creature_on_battlefield(&mut state, P1, "Ability Source", 1);
    let ability_stack_id = ObjectId(9996);
    state.stack.push_back(StackEntry {
        id: ability_stack_id,
        source_id: source,
        controller: P1,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ResolvedAbility::new(
                Effect::unimplemented("test", "V10 ability rung"),
                vec![],
                source,
                P1,
            )),
        },
    });
    let ability = ResolvedAbility::new(
        Effect::unimplemented("test", "V10 ability rung"),
        vec![TargetRef::Object(source)],
        source,
        P1,
    );
    assert_eq!(parent_target_controller(&ability, &state), Some(P1));
}

// ---------------------------------------------------------------------------
// V11 — Display shows the live controller, PER ENTRY
// ---------------------------------------------------------------------------

#[test]
fn stack_entry_detail_reports_the_live_controller() {
    let mut state = new_state();
    let caster = P1;
    let thief = P0;
    let spell = spell_object(&mut state, caster, "Displayed Spell", 1, Zone::Stack);
    push_spell_entry(&mut state, spell, caster, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, thief, "Threaten Source", 2);
    install_steal(&mut state, source, thief, spell);
    evaluate_layers(&mut state);

    let views = engine::game::derived_views::derive_views(&state, None);
    let display = views
        .stack_entry_details
        .get(&spell)
        .expect("the stack entry must have a display projection");
    assert_eq!(
        display.controller, thief,
        "StackEntryDisplay.controller must be the LIVE controller"
    );
    let entry = state.stack.back().unwrap();
    assert_eq!(
        entry.controller, caster,
        "the wire StackEntry.controller stays the by-default caster"
    );
}

/// SIBLING (V11): `group_key` is NOT extended — two coalesced look-alike
/// entries with different controllers each render their own display, which
/// is only possible because the field is per-entry.
#[test]
fn two_look_alike_entries_with_different_controllers_each_render_their_own() {
    let mut state = new_state();
    let a = spell_object(&mut state, P1, "Twin Spell", 1, Zone::Stack);
    let b = spell_object(&mut state, P1, "Twin Spell", 2, Zone::Stack);
    push_spell_entry(&mut state, a, P1, None, CastingVariant::Normal);
    push_spell_entry(&mut state, b, P1, None, CastingVariant::Normal);
    let source = creature_on_battlefield(&mut state, P0, "Threaten Source", 3);
    install_steal(&mut state, source, P0, a); // steal only ONE of the twins.
    evaluate_layers(&mut state);

    let views = engine::game::derived_views::derive_views(&state, None);
    assert_eq!(views.stack_entry_details[&a].controller, P0);
    assert_eq!(views.stack_entry_details[&b].controller, P1);
}

// ---------------------------------------------------------------------------
// MED-1 (review-impl round 1, charter revision 1) — a paused permanent
// spell's KEEP-classified riders (warp's CR 702.185a exile trigger, Room's
// CR 709.5d door designation) must read the BY-DEFAULT cast-time controller,
// matching the UNPAUSED path (`stack.rs` `entry.controller`), never the live
// controller the pause snapshot's `controller` field carries for resumption.
// ---------------------------------------------------------------------------

mod paused_permanent_spell_keeps_the_cast_time_controller_for_keep_riders {
    use super::*;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, EffectScope, ReplacementDefinition, ReplacementMode,
        TapStateChange,
    };
    use engine::types::actions::GameAction;
    use engine::types::mana::ManaCostShard;
    use engine::types::replacements::ReplacementEvent;

    /// A Warp creature spell on the stack, controlled at cast time by `caster`
    /// (P1), then STOLEN by `thief` (P0) via an `UntilEndOfTurn`
    /// `ChangeController` effect while it is still on the stack — so the LIVE
    /// controller (`stack_object_controller`, read by `resolve_top` before the
    /// pop) is the thief, while `entry.controller` (the by-default caster)
    /// stays P1. The creature also carries a single OPTIONAL self-ETB
    /// `ChangeZone` replacement ("may enter tapped") — CR 614.1a's lone-
    /// optional-candidate cause alone forces a genuine
    /// `ReplacementResult::NeedsChoice` pause, with NO `enters_under`
    /// controller-override anywhere in the fixture (MED-1 part B, the
    /// KEEP-consistency half).
    fn stolen_warp_creature_paused_on_its_own_optional_etb(card_num: u64) -> (GameState, ObjectId) {
        let mut state = new_state();
        let caster = P1;
        let thief = P0;
        let spell = create_object(
            &mut state,
            CardId(card_num),
            caster,
            "Stolen Warp Creature".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.keywords.push(Keyword::Warp(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            }));
            obj.base_keywords = obj.keywords.clone();
            let repl = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: TapStateChange::Tap,
                    },
                ));
            obj.replacement_definitions.push(repl.clone());
            std::sync::Arc::make_mut(&mut obj.base_replacement_definitions).push(repl);
            obj.base_characteristics_initialized = true;
        }
        push_spell_entry(&mut state, spell, caster, None, CastingVariant::Warp);
        let source = creature_on_battlefield(&mut state, thief, "Threaten Source", card_num + 100);
        install_steal(&mut state, source, thief, spell);
        evaluate_layers(&mut state);
        // REACH GUARD: the steal landed before resolution — the LIVE
        // controller genuinely diverges from the by-default caster.
        let entry = state.stack.back().unwrap().clone();
        assert_eq!(
            stack_object_controller(&state, &entry),
            thief,
            "fixture invariant: the steal must have applied before resolve_top runs"
        );
        assert_eq!(
            entry.controller, caster,
            "fixture invariant: entry.controller is unaffected by the steal (CR 112.2)"
        );
        (state, spell)
    }

    /// CR 702.185a + CR 603.7d + CR 608.2c: `warp_delayed_trigger_keeps_the_by_default_caster_through_a_pause`.
    /// REVERT-FAILING: reverting `engine_replacement.rs`'s `cast_time_controller`
    /// back to `ctx.controller` makes the delayed trigger's controller P0 (the
    /// thief / live controller) instead of P1 — proven below by temporarily
    /// reverting that exact line and observing this test fail (see the
    /// executor report; not re-run automatically here).
    #[test]
    fn warp_delayed_trigger_keeps_the_by_default_caster_through_a_pause() {
        let (mut state, spell) = stolen_warp_creature_paused_on_its_own_optional_etb(1);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // REACH GUARD: the spell left the stack (StackResolved fires even
        // though the replacement choice is pending) and a genuine
        // replacement-choice pause was raised — not an outright resolution.
        assert!(state.stack.is_empty(), "the spell must have left the stack");
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "expected a ReplacementChoice pause, got {:?}",
            state.waiting_for
        );
        let ctx = state
            .active_spell_resolution()
            .expect("the pause must stash a PendingSpellResolution");
        assert_eq!(
            ctx.controller, P0,
            "REACH GUARD: the stashed LIVE controller is the thief"
        );
        assert_eq!(
            ctx.cast_controller,
            Some(P1),
            "REACH GUARD: the stashed by-default cast_controller is the caster"
        );

        // Resume through the real production dispatcher (GameAction::ChooseReplacement,
        // routed through GameRunner::act -> apply_as_current -> engine_replacement::
        // handle_replacement_choice -> apply_pending_spell_resolution).
        let mut runner = GameRunner::from_state(state);
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("accept the optional ETB replacement");
        let state = runner.state();

        let trigger = state
            .delayed_triggers
            .iter()
            .find(|t| t.source_id == spell)
            .expect("the warp delayed trigger must have been installed");
        assert_eq!(
            trigger.controller, P1,
            "MED-1: the warp delayed trigger must carry the BY-DEFAULT caster \
             (matching the unpaused path's entry.controller), not the live \
             thief controller the spell resolved for"
        );
    }
}

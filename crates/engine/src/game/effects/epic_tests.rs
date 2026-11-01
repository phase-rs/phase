//! Tests for Epic (CR 702.50). Declared from `effects/mod.rs` so `epic.rs`
//! stays implementation-only.

use super::epic::{arm_epic, epic_upkeep_trigger, is_epic_locked, resolve};
use crate::analysis::resource::loop_states_equal_modulo_resources;
use crate::game::engine::apply;
use crate::game::layers::evaluate_layers;
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::game::stack::resolve_top;
use crate::game::triggers::check_delayed_triggers;
use crate::game::turns::execute_cleanup;
use crate::game::zones::{create_object, move_to_zone};
use crate::types::ability::{
    ContinuousModification, CostPaidObjectRecord, CostPaidObjectSnapshot, Duration, Effect,
    EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    loop_states_equal, CastOccurrence, CastingVariant, GameState, StackEntry, StackEntryKind,
    WaitingFor,
};
use crate::types::identifiers::{CardId, EpicEffectId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn gain_two_life() -> Effect {
    Effect::GainLife {
        amount: QuantityExpr::Fixed { value: 2 },
        player: TargetFilter::Controller,
    }
}

fn deal_damage_to_target_player() -> Effect {
    Effect::DealDamage {
        amount: QuantityExpr::Fixed { value: 3 },
        target: TargetFilter::Player,
        damage_source: None,
        excess: None,
    }
}

/// The resolved Epic spell's snapshot ability, sourced to `src`.
fn snapshot(src: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(gain_two_life(), Vec::new(), src, PlayerId(0))
}

fn targeted_snapshot(src: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        deal_damage_to_target_player(),
        vec![crate::types::ability::TargetRef::Player(PlayerId(1))],
        src,
        PlayerId(0),
    )
}

/// Arms `spell` as a P0 Epic effect of `proto` and returns the body of the
/// upkeep trigger the synthesis builds for it, naming the new entry.
fn armed_epic_copy(
    state: &mut GameState,
    proto: ObjectId,
    spell: ResolvedAbility,
) -> ResolvedAbility {
    let epic = arm_epic(state, proto, PlayerId(0), spell);
    *epic_upkeep_trigger(epic, &state.epic_effects[epic.0 as usize]).ability
}

/// An Epic card in the graveyard (the prototype the upkeep copies clone).
fn epic_card_in_graveyard(state: &mut GameState, card: u64) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        PlayerId(0),
        "Enduring Ideal".to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .keywords
        .push(Keyword::Epic);
    id
}

fn epic_spell_on_stack(state: &mut GameState, card: u64, is_token: bool) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        PlayerId(0),
        "Enduring Ideal".to_string(),
        Zone::Stack,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.is_token = is_token;
    obj.keywords.push(Keyword::Epic);
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(card),
            ability: Some(Box::new(snapshot(id))),
            casting_variant: CastingVariant::default(),
            actual_mana_spent: 0,
        },
    });
    id
}

#[test]
fn arm_epic_records_effect_and_locks_controller() {
    // CR 702.50a-b: arming records a rest-of-game effect and locks the caster.
    let mut state = GameState::new_two_player(42);
    let src = epic_card_in_graveyard(&mut state, 1);

    assert!(!is_epic_locked(&state, PlayerId(0)));
    arm_epic(&mut state, src, PlayerId(0), snapshot(src));

    assert_eq!(state.epic_effects.len(), 1);
    let effect = &state.epic_effects[0];
    assert_eq!(effect.controller, PlayerId(0));
    assert_eq!(effect.prototype_id, src);
    // CR 702.50b: the controller can no longer cast spells; the opponent can.
    assert!(is_epic_locked(&state, PlayerId(0)));
    assert!(!is_epic_locked(&state, PlayerId(1)));
}

#[test]
fn each_epic_resolution_records_an_independent_effect() {
    // CR 702.50a: two Epic spells → two independent rest-of-game effects.
    let mut state = GameState::new_two_player(42);
    let a = epic_card_in_graveyard(&mut state, 1);
    let b = epic_card_in_graveyard(&mut state, 2);
    arm_epic(&mut state, a, PlayerId(0), snapshot(a));
    arm_epic(&mut state, b, PlayerId(0), snapshot(b));
    assert_eq!(state.epic_effects.len(), 2);
}

#[test]
fn game_state_equality_includes_epic_effects() {
    // CR 702.50a-b: Epic effects are persistent game state: they change cast
    // legality and future upkeep triggers, so search/dedup equality must see
    // them.
    let mut with_epic = GameState::new_two_player(42);
    let mut without_epic = GameState::new_two_player(42);
    let src_with_epic = epic_card_in_graveyard(&mut with_epic, 1);
    let _src_without_epic = epic_card_in_graveyard(&mut without_epic, 1);

    assert_eq!(with_epic, without_epic);
    arm_epic(
        &mut with_epic,
        src_with_epic,
        PlayerId(0),
        snapshot(src_with_epic),
    );

    assert_ne!(with_epic, without_epic);
}

#[test]
fn epic_effect_survives_end_of_turn_cleanup() {
    // CR 702.50a: the rest-of-game effect must NOT be purged at cleanup. (A
    // stored recurring delayed trigger would be — `turns.rs` keeps only
    // `one_shot && !WhenNextEvent` — which is why Epic is a persistent
    // collection instead.)
    let mut state = GameState::new_two_player(42);
    let src = epic_card_in_graveyard(&mut state, 1);
    arm_epic(&mut state, src, PlayerId(0), snapshot(src));
    assert_eq!(state.epic_effects.len(), 1);

    let mut events = Vec::new();
    execute_cleanup(&mut state, &mut events);

    // The effect — and therefore the cast lock and future upkeep copies —
    // survives into later turns.
    assert_eq!(state.epic_effects.len(), 1, "Epic survives cleanup");
    assert!(is_epic_locked(&state, PlayerId(0)));
}

#[test]
fn epic_fires_a_copy_trigger_at_the_controllers_upkeep_and_recurs() {
    // CR 702.50a + CR 707.10: the synthesized upkeep trigger follows the real
    // delayed-trigger stack path and resolves into a copied spell on the stack.
    let mut state = GameState::new_two_player(42);
    let src = epic_card_in_graveyard(&mut state, 7);
    arm_epic(&mut state, src, PlayerId(0), snapshot(src));

    state.active_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    let fired = check_delayed_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    assert!(!fired.is_empty(), "an EpicCopy trigger fires at the upkeep");
    let top = state
        .stack
        .back()
        .expect("EpicCopy trigger is on the stack");
    assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));

    let mut resolve_events = Vec::new();
    resolve_top(&mut state, &mut resolve_events);
    let copy = state.stack.back().expect("Epic spell copy is on the stack");
    assert!(matches!(copy.kind, StackEntryKind::Spell { .. }));
    assert!(resolve_events
        .iter()
        .any(|e| matches!(e, GameEvent::SpellCopied { .. })));

    // CR 702.50a "for the rest of the game" — the effect recurs (not consumed).
    assert_eq!(state.epic_effects.len(), 1);

    // Firing again at a later upkeep still fires — proving recurrence.
    let fired_again = check_delayed_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    assert!(!fired_again.is_empty(), "Epic fires again the next upkeep");
}

#[test]
fn epic_does_not_fire_at_an_opponents_upkeep() {
    // CR 702.50a: copies happen on the controller's upkeeps only.
    let mut state = GameState::new_two_player(42);
    let src = epic_card_in_graveyard(&mut state, 7);
    arm_epic(&mut state, src, PlayerId(0), snapshot(src));

    state.active_player = PlayerId(1);
    state.phase = Phase::Upkeep;
    let fired = check_delayed_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    assert!(
        fired.is_empty(),
        "Epic must not fire at the opponent's upkeep"
    );
}

#[test]
fn epic_copy_puts_a_keyword_stripped_copy_on_the_stack() {
    // CR 702.50a + CR 707.10: resolving EpicCopy puts a copy of the spell on the
    // stack, excluding the epic ability so it does not recurse.
    let mut state = GameState::new_two_player(42);
    let proto = epic_card_in_graveyard(&mut state, 7);

    let epic_ability = armed_epic_copy(&mut state, proto, snapshot(proto));

    let mut events = Vec::new();
    resolve(&mut state, &epic_ability, &mut events).expect("EpicCopy resolves");

    let top = state.stack.back().expect("a copy is on the stack");
    let copy_id = top.id;
    assert!(matches!(top.kind, StackEntryKind::Spell { .. }));

    // CR 707.10: the copy is a token spell; CR 702.50a: Epic is stripped so the
    // copy's own resolution won't arm a second Epic effect.
    let copy = state.objects.get(&copy_id).expect("copy object exists");
    assert!(copy.is_token);
    assert!(
        !copy.keywords.iter().any(|k| matches!(k, Keyword::Epic)),
        "the copy must exclude the epic ability"
    );

    // CR 707.10: a copy is placed, not cast — SpellCopied, not SpellCast.
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::SpellCopied { .. })));
    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::SpellCast { .. })));
}

#[test]
fn copied_epic_spell_resolution_still_arms_epic() {
    // CR 702.50a-b: an ordinary copy of an Epic spell still has Epic and, when
    // that copied spell resolves, creates the same rest-of-game effects. Only
    // Epic-generated upkeep copies strip Epic to avoid recursion.
    let mut state = GameState::new_two_player(42);
    let copied_spell = epic_spell_on_stack(&mut state, 7, true);

    let mut events = Vec::new();
    resolve_top(&mut state, &mut events);

    assert_eq!(state.epic_effects.len(), 1);
    assert_eq!(state.epic_effects[0].prototype_id, copied_spell);
    assert!(is_epic_locked(&state, PlayerId(0)));
}

#[test]
fn targeted_epic_copy_opens_copy_retarget_choice() {
    // CR 702.50a + CR 707.10c: targeted Epic copies grant the same optional
    // retargeting choice as other "you may choose new targets" copy effects.
    let mut state = GameState::new_two_player(42);
    let proto = epic_card_in_graveyard(&mut state, 7);

    let epic_ability = armed_epic_copy(&mut state, proto, targeted_snapshot(proto));

    let mut events = Vec::new();
    resolve(&mut state, &epic_ability, &mut events).expect("EpicCopy resolves");

    let WaitingFor::CopyRetarget {
        player,
        copy_id,
        target_slots,
        effect_kind,
        effect_source_id,
        current_slot,
        ..
    } = &state.waiting_for
    else {
        panic!("expected CopyRetarget, got {:?}", state.waiting_for);
    };

    assert_eq!(*player, PlayerId(0));
    assert_eq!(*current_slot, 0);
    assert_eq!(*effect_kind, EffectKind::EpicCopy);
    assert_eq!(*effect_source_id, Some(proto));
    assert_eq!(state.stack.back().map(|entry| entry.id), Some(*copy_id));
    assert_eq!(target_slots.len(), 1);
    assert_eq!(
        target_slots[0].current,
        Some(crate::types::ability::TargetRef::Player(PlayerId(1)))
    );
}

#[test]
fn targeted_epic_retarget_completion_resolves_as_epic_copy() {
    // CR 702.50a + CR 707.10c: the shared retarget action path must complete
    // the deferred EpicCopy effect, not the generic CopySpell effect.
    let mut state = GameState::new_two_player(42);
    let proto = epic_card_in_graveyard(&mut state, 7);

    let epic_ability = armed_epic_copy(&mut state, proto, targeted_snapshot(proto));

    let mut events = Vec::new();
    resolve(&mut state, &epic_ability, &mut events).expect("EpicCopy opens retarget choice");

    let result = apply(&mut state, PlayerId(0), GameAction::KeepAllCopyTargets)
        .expect("retarget choice completes");

    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::EpicCopy,
            source_id,
        ..} if *source_id == proto
    )));
    assert!(!result.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::CopySpell,
            ..
        }
    )));
}

#[test]
fn legacy_copy_retarget_completion_uses_copy_as_default_source() {
    // CR 707.10c: pre-metadata CopyRetarget saves omitted completion metadata;
    // those states are generic copy choices whose completion source is the copy.
    let mut state = GameState::new_two_player(42);
    let copy_id = epic_spell_on_stack(&mut state, 7, true);
    // The copy holds the target its legacy prompt records.
    if let Some(ability) = state.stack.back_mut().and_then(|entry| entry.ability_mut()) {
        *ability = targeted_snapshot(copy_id);
    }

    state.waiting_for = WaitingFor::CopyRetarget {
        player: PlayerId(0),
        controller: None,
        copy_id,
        target_slots: vec![crate::types::game_state::CopyTargetSlot {
            current: Some(crate::types::ability::TargetRef::Player(PlayerId(1))),
            legal_alternatives: Vec::new(),
            address: None,
            can_keep: false,
            can_decline: false,
        }],
        effect_kind: EffectKind::CopySpell,
        effect_source_id: None,
        current_slot: 0,
        paradigm_remaining_offers: None,
        mode: None,
        picks: None,
        can_keep_rest: false,
        announcer_election: None,
    };

    let result = apply(&mut state, PlayerId(0), GameAction::KeepAllCopyTargets)
        .expect("legacy retarget choice completes");

    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::CopySpell,
            source_id,
        ..} if *source_id == copy_id
    )));
}

#[test]
fn epic_copy_is_a_noop_when_the_prototype_is_gone() {
    // CR 608.2: with no last-known prototype object, no copy can be built.
    let mut state = GameState::new_two_player(42);
    let missing = ObjectId(9999);
    let epic_ability = armed_epic_copy(&mut state, missing, snapshot(missing));

    let mut events = Vec::new();
    resolve(&mut state, &epic_ability, &mut events).expect("resolves as a no-op");
    assert!(state.stack.is_empty(), "no copy is created");
}

// ---------------------------------------------------------------------------
// The upkeep copy names its Epic effect by handle (`EpicEffectId`).
// ---------------------------------------------------------------------------

/// Endless Swarm's Oracle text, verbatim (Scryfall and MTGJSON agree).
const ENDLESS_SWARM: &str = "Create a 1/1 green Snake creature token for each card in your hand.\nEpic (For the rest of the game, you can't cast spells. At the beginning of each of your upkeeps, copy this spell except for its epic ability.)";

/// A sorcery in P0's graveyard: the prototype an upkeep copy clones.
fn epic_sorcery_in_graveyard(state: &mut GameState, card: u64, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        PlayerId(0),
        name.to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Sorcery);
    id
}

/// Fires P0's upkeep delayed triggers through the production synthesis.
fn fire_p0_upkeep(state: &mut GameState) {
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    check_delayed_triggers(
        state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
}

/// The `EpicCopy` handle `ability` names.
fn handle_of(ability: &ResolvedAbility) -> EpicEffectId {
    match ability.effect {
        Effect::EpicCopy { epic } => epic,
        ref other => panic!("expected an EpicCopy body, got {other:?}"),
    }
}

/// The handles of the triggers waiting in the CR 603.3b ordering prompt,
/// sorted.
fn ordering_handles(state: &GameState) -> Vec<EpicEffectId> {
    let mut handles: Vec<EpicEffectId> = state
        .pending_trigger_order
        .as_ref()
        .expect("reach-guard: an ordering pass is pending")
        .groups
        .iter()
        .flat_map(|group| group.triggers.iter())
        .map(|ctx| handle_of(&ctx.pending.ability))
        .collect();
    handles.sort();
    handles
}

/// The handles of the triggered `EpicCopy` entries on the stack, bottom first.
fn stack_handles(state: &GameState) -> Vec<EpicEffectId> {
    state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .filter_map(StackEntry::ability)
        .map(handle_of)
        .collect()
}

/// V3's board: two P0 Epic effects from two prototypes with different
/// snapshots, fired at P0's upkeep and stopped at the CR 603.3b prompt.
fn distinct_snapshot_board() -> (GameState, ObjectId, ObjectId) {
    let mut state = GameState::new_two_player(42);
    let gain = epic_sorcery_in_graveyard(&mut state, 1, "Gain Prototype");
    let damage = epic_sorcery_in_graveyard(&mut state, 2, "Damage Prototype");
    arm_epic(&mut state, gain, PlayerId(0), snapshot(gain));
    arm_epic(&mut state, damage, PlayerId(0), targeted_snapshot(damage));
    fire_p0_upkeep(&mut state);
    (state, gain, damage)
}

/// Resolves the top triggered `EpicCopy`, returning the copy it pushed as
/// `(copy name, copy effect kind)`, then resolves that copy (keeping its
/// targets when CR 707.10c offers new ones).
fn resolve_epic_trigger_and_copy(state: &mut GameState) -> (String, EffectKind) {
    let mut events = Vec::new();
    resolve_top(state, &mut events);
    let copy = state.stack.back().expect("the copy is on the stack");
    assert!(matches!(copy.kind, StackEntryKind::Spell { .. }));
    let pushed = (
        state.objects[&copy.id].name.clone(),
        EffectKind::from(&copy.ability().expect("the copy carries its ability").effect),
    );
    if matches!(state.waiting_for, WaitingFor::CopyRetarget { .. }) {
        apply(state, PlayerId(0), GameAction::KeepAllCopyTargets)
            .expect("the retarget choice completes");
    }
    resolve_top(state, &mut events);
    pushed
}

/// V3: each upkeep copy resolves the snapshot its own handle names.
#[test]
fn each_epic_effect_copies_its_own_snapshot() {
    let (mut state, _, _) = distinct_snapshot_board();
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::OrderTriggers { player, ref triggers } if player == PlayerId(0) && triggers.len() == 2
        ),
        "CR 603.3b: distinct prototypes with distinct snapshots prompt, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        ordering_handles(&state),
        vec![EpicEffectId(0), EpicEffectId(1)],
        "reach-guard: two triggers fired, each naming its own entry"
    );
    apply(
        &mut state,
        PlayerId(0),
        GameAction::OrderTriggers { order: vec![1, 0] },
    )
    .expect("the ordering is accepted");

    let copies = [
        resolve_epic_trigger_and_copy(&mut state),
        resolve_epic_trigger_and_copy(&mut state),
    ];
    for expected in [
        ("Gain Prototype".to_string(), EffectKind::GainLife),
        ("Damage Prototype".to_string(), EffectKind::DealDamage),
    ] {
        assert!(
            copies.contains(&expected),
            "CR 707.10: each copy clones its own prototype and resolves its own snapshot: \
             {copies:?}"
        );
    }
}

/// V4: the synthesized trigger resolves into a copy whose body is the stored
/// snapshot's effect.
#[test]
fn epic_upkeep_copy_resolves_into_the_snapshot_effect() {
    let mut state = GameState::new_two_player(42);
    let src = epic_sorcery_in_graveyard(&mut state, 7, "Epic Sorcery");
    arm_epic(&mut state, src, PlayerId(0), snapshot(src));
    fire_p0_upkeep(&mut state);
    let life_before = state.players[0].life;

    assert_eq!(
        resolve_epic_trigger_and_copy(&mut state),
        ("Epic Sorcery".to_string(), EffectKind::GainLife)
    );
    assert_eq!(state.players[0].life, life_before + 2);
    assert!(state.stack.is_empty());
    assert_eq!(state.epic_effects.len(), 1);
}

/// V5 (CR 707.10): the copy carries no cast occurrence at any node and writes
/// no cast record, and resolving it leaves the stored snapshot untouched.
#[test]
fn epic_copy_has_no_cast_occurrence_and_leaves_the_stored_snapshot_unchanged() {
    let mut state = GameState::new_two_player(42);
    let src = epic_sorcery_in_graveyard(&mut state, 7, "Epic Sorcery");
    let mut spell = snapshot(src);
    spell.sub_ability = Some(Box::new(snapshot(src)));
    spell.set_cast_occurrence_recursive(Some(CastOccurrence {
        caster: PlayerId(0),
        turn_journal_index: 0,
    }));
    arm_epic(&mut state, src, PlayerId(0), spell);
    let stored = state.epic_effects[0].clone();
    let cast_records = state.spells_cast_this_turn_by_player.clone();
    fire_p0_upkeep(&mut state);

    let mut events = Vec::new();
    resolve_top(&mut state, &mut events);
    let copy = state.stack.back().and_then(StackEntry::ability).unwrap();
    assert!(
        copy.sub_ability.is_some(),
        "reach-guard: the copy carries the two-node graph"
    );
    fn graph_is_clear(ability: &ResolvedAbility) -> bool {
        ability.cast_occurrence.is_none()
            && ability.sub_ability.as_deref().is_none_or(graph_is_clear)
            && ability.else_ability.as_deref().is_none_or(graph_is_clear)
    }
    assert!(graph_is_clear(copy), "CR 707.10: a copy is not cast");
    assert_eq!(state.spells_cast_this_turn_by_player, cast_records);
    assert_eq!(state.epic_effects[0], stored);
}

/// V6: the trigger body is a quantity-walk leaf; its snapshot's quantities are
/// evaluated in the copy's own resolution.
#[test]
fn epic_copy_is_a_quantity_walk_leaf() {
    let mut state = GameState::new_two_player(42);
    let src = epic_card_in_graveyard(&mut state, 7);
    let epic = arm_epic(&mut state, src, PlayerId(0), snapshot(src));
    let mut snapshot_quantities = 0;
    state.epic_effects[0]
        .spell
        .effect
        .for_each_quantity_expr(&mut |_| snapshot_quantities += 1);
    assert_eq!(snapshot_quantities, 1, "reach-guard: the snapshot has one");

    let mut trigger = epic_upkeep_trigger(epic, &state.epic_effects[0]);
    let mut visited = 0;
    trigger
        .ability
        .effect
        .for_each_quantity_expr(&mut |_| visited += 1);
    trigger
        .ability
        .effect
        .for_each_quantity_expr_mut(&mut |_| visited += 1);
    assert_eq!(visited, 0);
}

/// V19: a handle that names no entry is loud in a debug build.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "EpicCopy names no Epic effect")]
fn epic_copy_naming_no_entry_panics_in_debug() {
    let mut state = GameState::new_two_player(42);
    let src = epic_card_in_graveyard(&mut state, 7);
    let mut armed = state.clone();
    let epic = arm_epic(&mut armed, src, PlayerId(0), snapshot(src));
    let trigger = epic_upkeep_trigger(epic, &armed.epic_effects[0]);
    let mut events = Vec::new();
    let _ = resolve(&mut state, &trigger.ability, &mut events);
}

/// V22: trigger equality follows the handle.
#[test]
fn in_flight_epic_copy_equality_follows_its_handle() {
    let (mut state, _, _) = distinct_snapshot_board();
    apply(
        &mut state,
        PlayerId(0),
        GameAction::OrderTriggers { order: vec![0, 1] },
    )
    .expect("the ordering is accepted");
    let handles = stack_handles(&state);
    assert_eq!(
        handles.len(),
        2,
        "reach-guard: both triggers are on the stack"
    );
    let loop_equal = |a: &GameState, b: &GameState| {
        loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop())
    };
    assert!(state == state.clone() && loop_equal(&state, &state.clone()));

    let mut other_snapshot = state.clone();
    other_snapshot.epic_effects[1].spell.effect = Effect::NoOp;
    assert!(state != other_snapshot && !loop_equal(&state, &other_snapshot));

    let mut other_handle = state.clone();
    let renamed = other_handle
        .stack
        .iter_mut()
        .filter_map(StackEntry::ability_mut)
        .find(|ability| handle_of(ability) == EpicEffectId(0))
        .expect("a trigger names entry 0");
    renamed.effect = Effect::EpicCopy {
        epic: EpicEffectId(1),
    };
    assert!(state != other_handle && !loop_equal(&state, &other_handle));
}

/// V23 (CR 104.4b): loop normalization leaves a stored snapshot's cast
/// coordinate as it is, so two samples whose stored entries differ there
/// compare unequal.
#[test]
fn loop_comparison_of_an_in_flight_epic_copy_is_unchanged() {
    fn sample(turn_journal_index: u32) -> GameState {
        let mut state = GameState::new_two_player(42);
        let src = epic_sorcery_in_graveyard(&mut state, 7, "Epic Sorcery");
        let mut spell = snapshot(src);
        spell.set_cast_occurrence_recursive(Some(CastOccurrence {
            caster: PlayerId(0),
            turn_journal_index,
        }));
        arm_epic(&mut state, src, PlayerId(0), spell);
        fire_p0_upkeep(&mut state);
        state
    }
    let loop_equal = |a: &GameState, b: &GameState| {
        loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop())
    };
    let a = sample(3);
    assert_eq!(stack_handles(&a), vec![EpicEffectId(0)], "reach-guard");
    assert!(loop_equal(&a, &sample(3)));
    assert!(!loop_equal(&a, &sample(4)));
}

// --- Runner boards -----------------------------------------------------------

/// A scenario at P0's precombat main with both libraries seeded, so the
/// runner's draw steps cannot end the game.
fn epic_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        let names: Vec<String> = (0..8)
            .map(|i| format!("Library {} {i}", player.0))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_library_top(player, &refs);
    }
    scenario
}

/// Passes priority, declaring no attackers when asked, until `stop` holds.
/// Any other prompt before `stop` fails the test.
fn advance_until(runner: &mut GameRunner, stop: impl Fn(&GameState) -> bool) {
    for _ in 0..400 {
        if stop(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: Vec::new(),
                bands: Vec::new(),
            },
            other => panic!("unexpected prompt before the stop condition: {other:?}"),
        };
        runner.act(action).expect("the advance action is accepted");
    }
    panic!("the stop condition was not reached within the step budget");
}

/// Stops at `player`'s next upkeep once a trigger is on the stack or awaits an
/// order.
fn at_next_upkeep_of(player: PlayerId, after_turn: u32) -> impl Fn(&GameState) -> bool {
    move |state| {
        state.turn_number > after_turn
            && state.active_player == player
            && state.phase == Phase::Upkeep
            && (!state.stack.is_empty()
                || matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }))
    }
}

fn snakes_of(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[*id];
            object.controller == player && object.name.contains("Snake")
        })
        .count()
}

/// P1 gains control of the committed spell on the stack (the
/// `spell_controller_is_derived.rs::install_steal` fixture shape).
fn steal_on_stack(state: &mut GameState, thief_source: ObjectId, spell: ObjectId) {
    state.add_transient_continuous_effect(
        thief_source,
        P1,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: spell },
        vec![ContinuousModification::ChangeController],
        None,
    );
    evaluate_layers(state);
}

/// The equal-snapshot board (`filler_first == false`): P0 casts Endless Swarm,
/// P1 gains control of it on the stack, it resolves; it returns to P0's hand,
/// and on P0's next turn the same is done again with the turn's first spell,
/// so both entries hold `==` snapshots. With `filler_first`, P0 casts a filler
/// sorcery first, so the second snapshot's cast coordinate has journal index 1.
/// Returns the runner at P0's second main phase after the second resolution.
fn stolen_twice(filler_first: bool) -> (GameRunner, ObjectId) {
    let mut scenario = epic_scenario();
    let swarm = scenario
        .add_spell_to_hand(P0, "Endless Swarm", false)
        .from_oracle_text_with_keywords(&["Epic"], ENDLESS_SWARM)
        .id();
    let filler = scenario
        .add_spell_to_hand_from_oracle(P0, "Filler Sorcery", false, "You gain 1 life.")
        .id();
    scenario.with_cards_in_hand(P0, &["Filler A"]);
    let thief_source = scenario.add_creature(P1, "Threaten Source", 1, 1).id();
    let mut runner = scenario.build();

    let mut commit = runner.cast(swarm).commit();
    steal_on_stack(commit.state_mut(), thief_source, swarm);
    commit.resolve();
    move_to_zone(runner.state_mut(), swarm, Zone::Hand, &mut Vec::new());
    let first_turn = runner.state().turn_number;
    advance_until(&mut runner, |state| {
        state.turn_number >= first_turn + 2
            && state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });

    if filler_first {
        runner.cast(filler).resolve();
    }
    let mut commit = runner.cast(swarm).commit();
    let second_cast = commit
        .state()
        .stack
        .back()
        .and_then(StackEntry::ability)
        .and_then(|ability| ability.cast_occurrence);
    assert_eq!(
        second_cast,
        Some(CastOccurrence {
            caster: P0,
            turn_journal_index: u32::from(filler_first),
        }),
        "reach-guard: the second cast's coordinate"
    );
    steal_on_stack(commit.state_mut(), thief_source, swarm);
    commit.resolve();

    let entries: Vec<_> = runner
        .state()
        .epic_effects
        .iter()
        .map(|effect| (effect.controller, effect.prototype_id))
        .collect();
    assert_eq!(
        entries,
        vec![(P1, swarm), (P1, swarm)],
        "reach-guard: both resolutions armed Epic under the thief"
    );
    (runner, swarm)
}

/// Exchanges the contents of the two stack entries (keeping their ids and
/// positions): the stack the other push order builds.
fn other_order(state: &GameState) -> GameState {
    let mut other = state.clone();
    let (bottom, top) = (other.stack[0].clone(), other.stack[1].clone());
    for (slot, from) in [(0, &top), (1, &bottom)] {
        other.stack[slot].source_id = from.source_id;
        other.stack[slot].controller = from.controller;
        other.stack[slot].kind = from.kind.clone();
    }
    other
}

/// A clone whose two stack triggers' handles are exchanged.
fn swapped_stack_handles(state: &GameState) -> GameState {
    let mut swapped = state.clone();
    let handles = stack_handles(state);
    for (ability, handle) in swapped
        .stack
        .iter_mut()
        .filter_map(StackEntry::ability_mut)
        .zip(handles.into_iter().rev())
    {
        ability.effect = Effect::EpicCopy { epic: handle };
    }
    swapped
}

/// V21 (production entry): Endless Swarm cast through the runner copies itself
/// at its caster's next upkeep.
#[test]
fn endless_swarm_copies_itself_at_the_casters_next_upkeep() {
    let mut scenario = epic_scenario();
    let swarm = scenario
        .add_spell_to_hand(P0, "Endless Swarm", false)
        .from_oracle_text_with_keywords(&["Epic"], ENDLESS_SWARM)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            generic: 5,
        })
        .id();
    scenario.with_cards_in_hand(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|i| ManaUnit::new(ManaType::Green, ObjectId(9000 + i), false, Vec::new()))
            .collect(),
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(swarm).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let state = outcome.state();
    assert!(state.stack.is_empty());
    assert_eq!(state.epic_effects.len(), 1);
    assert_eq!(snakes_of(state, P0), 3, "one Snake per card in hand");
    assert_eq!(state.objects[&swarm].zone, Zone::Graveyard);

    let cast_turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P0, cast_turn));
    let top = runner
        .state()
        .stack
        .back()
        .expect("a trigger is on the stack");
    assert!(
        matches!(top.kind, StackEntryKind::TriggeredAbility { .. })
            && top.controller == P0
            && top.source_id == swarm
            && matches!(
                top.ability().map(|ability| &ability.effect),
                Some(Effect::EpicCopy { .. })
            ),
        "reach-guard: CR 702.50a's upkeep copy trigger, got {top:?}"
    );
    let upkeep_turn = runner.state().turn_number;
    advance_until(&mut runner, |state| {
        state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
    let state = runner.state();
    assert_eq!(
        (state.turn_number, state.phase),
        (upkeep_turn, Phase::Upkeep)
    );
    assert_eq!(
        snakes_of(state, P0),
        6,
        "CR 707.10: the copy counts the three cards in hand"
    );
    assert_eq!(state.epic_effects.len(), 1);
}

/// V24 (CR 603.3b): a same-prototype group with equal snapshots shares one
/// handle and auto-orders.
#[test]
fn equal_snapshot_epic_group_auto_orders() {
    let (mut runner, swarm) = stolen_twice(false);
    assert_eq!(
        runner.state().epic_effects[0].spell,
        runner.state().epic_effects[1].spell,
        "reach-guard: the two snapshots are equal"
    );
    let turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P1, turn));
    let state = runner.state();
    // No CR 603.3b prompt: the advance would have stopped at `OrderTriggers`.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(stack_handles(state), vec![EpicEffectId(0), EpicEffectId(0)]);
    assert!(state
        .stack
        .iter()
        .all(|entry| entry.controller == P1 && entry.source_id == swarm));

    let hand = state.players[1].hand.len();
    assert!(hand > 0, "reach-guard: each copy has a card to count");
    let snakes = snakes_of(state, P1);
    advance_until(&mut runner, |state| {
        state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        snakes_of(runner.state(), P1),
        snakes + 2 * hand,
        "each copy creates one Snake per card in P1's hand"
    );
}

/// V26: a re-cast whose snapshot differs only in the cast coordinate's journal
/// index gets its own handle and prompts for an order.
#[test]
fn endless_swarm_recast_after_another_spell_still_prompts() {
    let (mut runner, _) = stolen_twice(true);
    assert_ne!(
        runner.state().epic_effects[0].spell,
        runner.state().epic_effects[1].spell,
        "reach-guard: the snapshots differ in the cast coordinate"
    );
    let turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P1, turn));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OrderTriggers { player: P1, ref triggers } if triggers.len() == 2
    ));
    assert_eq!(
        ordering_handles(runner.state()),
        vec![EpicEffectId(0), EpicEffectId(1)]
    );
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .expect("the ordering is accepted");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    assert_eq!(stack_handles(runner.state()).len(), 2);
}

/// Arms one P0 Epic effect per `(prototype, snapshot)` built by `entries` on a
/// seeded scenario, then advances to P0's next upkeep.
fn armed_board(
    entries: impl FnOnce(&mut GameState) -> Vec<(ObjectId, ResolvedAbility)>,
) -> GameRunner {
    let mut runner = epic_scenario().build();
    for (prototype, spell) in entries(runner.state_mut()) {
        arm_epic(runner.state_mut(), prototype, P0, spell);
    }
    let turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P0, turn));
    runner
}

/// Board H: one prototype, snapshots equal except the second's
/// `source_incarnation`.
fn identity_only_board() -> GameRunner {
    armed_board(|state| {
        let proto = epic_sorcery_in_graveyard(state, 1, "Epic Sorcery");
        let mut second = snapshot(proto);
        second.source_incarnation = Some(7);
        vec![(proto, snapshot(proto)), (proto, second)]
    })
}

/// Board K: one prototype, `==` snapshots whose one cost record names the same
/// object at two incarnations (captured before and after it changed zones).
fn cost_record_board() -> GameRunner {
    armed_board(|state| {
        let proto = epic_sorcery_in_graveyard(state, 1, "Epic Sorcery");
        let fodder = create_object(
            state,
            CardId(3),
            PlayerId(0),
            "Exiled Fodder".to_string(),
            Zone::Battlefield,
        );
        let capture = |state: &GameState| {
            let object = &state.objects[&fodder];
            CostPaidObjectRecord::Captured(CostPaidObjectSnapshot::capture(
                object,
                object.snapshot_public_characteristics(),
            ))
        };
        let before = capture(state);
        move_to_zone(state, fodder, Zone::Exile, &mut Vec::new());
        let after = capture(state);
        [before, after]
            .into_iter()
            .map(|record| {
                let mut spell = snapshot(proto);
                spell.add_cost_paid_records_recursive(&[record]);
                (proto, spell)
            })
            .collect()
    })
}

fn submit_order(runner: &mut GameRunner) {
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .expect("the ordering is accepted");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// V25 (CR 603.3b): interning merges exactly the snapshots that resolve
/// identically. A same-prototype group prompts when its snapshots differ by
/// effect (E), by trigger identity alone (H) or by cost-record incarnation
/// alone (K), and auto-orders when they are equal (E0).
#[test]
fn same_prototype_epic_group_prompts_only_for_identical_snapshots() {
    let prompts = |runner: &GameRunner| {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OrderTriggers { player: P0, ref triggers } if triggers.len() == 2
            ),
            "expected a CR 603.3b prompt, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(
            ordering_handles(runner.state()),
            vec![EpicEffectId(0), EpicEffectId(1)]
        );
    };
    let one_prototype = |runner: &GameRunner| {
        let effects = &runner.state().epic_effects;
        assert_eq!(effects.len(), 2, "reach-guard");
        assert_eq!(
            effects[0].prototype_id, effects[1].prototype_id,
            "reach-guard"
        );
    };

    // E: different effects.
    let runner = armed_board(|state| {
        let proto = epic_sorcery_in_graveyard(state, 1, "Epic Sorcery");
        vec![(proto, snapshot(proto)), (proto, targeted_snapshot(proto))]
    });
    one_prototype(&runner);
    assert_ne!(
        runner.state().epic_effects[0].spell,
        runner.state().epic_effects[1].spell
    );
    prompts(&runner);

    // H: trigger identity alone.
    let runner = identity_only_board();
    one_prototype(&runner);
    let effects = &runner.state().epic_effects;
    assert_ne!(effects[0].spell, effects[1].spell, "reach-guard");
    let mut cleared = effects[1].spell.clone();
    cleared.source_incarnation = None;
    assert_eq!(
        effects[0].spell, cleared,
        "reach-guard: only source_incarnation differs"
    );
    prompts(&runner);

    // K: cost-record incarnation alone, inside `==`'s relaxation.
    let mut runner = cost_record_board();
    one_prototype(&runner);
    let effects = &runner.state().epic_effects;
    assert_eq!(
        effects[0].spell, effects[1].spell,
        "reach-guard: `==` relaxes the records"
    );
    assert_ne!(
        effects[0].spell.cost_paid_objects, effects[1].spell.cost_paid_objects,
        "reach-guard: the records name different incarnations"
    );
    prompts(&runner);
    submit_order(&mut runner);
    let mut copy_records = Vec::new();
    for _ in 0..2 {
        let state = runner.state_mut();
        let epic = handle_of(state.stack.back().and_then(StackEntry::ability).unwrap());
        let mut events = Vec::new();
        resolve_top(state, &mut events);
        let copy = state.stack.back().expect("the copy is on the stack");
        assert!(matches!(copy.kind, StackEntryKind::Spell { .. }));
        let records = copy.ability().unwrap().cost_paid_objects.clone();
        assert_eq!(
            records, state.epic_effects[epic.0 as usize].spell.cost_paid_objects,
            "CR 707.10: the copy uses the objects paid for its own original"
        );
        copy_records.push(records);
        resolve_top(state, &mut events);
    }
    assert_ne!(copy_records[0], copy_records[1]);

    // E0 control: equal snapshots, no cost records.
    let runner = armed_board(|state| {
        let proto = epic_sorcery_in_graveyard(state, 1, "Epic Sorcery");
        vec![(proto, snapshot(proto)), (proto, snapshot(proto))]
    });
    one_prototype(&runner);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert_eq!(
        stack_handles(runner.state()),
        vec![EpicEffectId(0), EpicEffectId(0)]
    );
}

/// V10 (CR 603.3b): equal snapshots of distinct prototypes share one handle,
/// still prompt (the prototypes differ), and each copy clones its own
/// prototype.
#[test]
fn epic_group_of_distinct_prototypes_with_equal_snapshots_prompts() {
    let mut runner = armed_board(|state| {
        let a = epic_sorcery_in_graveyard(state, 1, "Proto A");
        let b = epic_sorcery_in_graveyard(state, 2, "Proto B");
        let spell = snapshot(a);
        vec![(a, spell.clone()), (b, spell)]
    });
    let effects = &runner.state().epic_effects;
    assert_eq!(effects.len(), 2, "reach-guard");
    assert_ne!(
        effects[0].prototype_id, effects[1].prototype_id,
        "reach-guard"
    );
    assert_eq!(effects[0].spell, effects[1].spell, "reach-guard");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OrderTriggers { player: P0, ref triggers } if triggers.len() == 2
    ));
    assert_eq!(
        ordering_handles(runner.state()),
        vec![EpicEffectId(0), EpicEffectId(0)]
    );
    submit_order(&mut runner);

    let mut copies = std::collections::BTreeMap::new();
    while !runner.state().stack.is_empty() {
        let top = runner.state().stack.back().unwrap();
        if matches!(top.kind, StackEntryKind::Spell { .. }) {
            copies.insert(top.id, runner.state().objects[&top.id].name.clone());
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    let mut copy_names: Vec<String> = copies.into_values().collect();
    copy_names.sort();
    assert_eq!(
        copy_names,
        vec!["Proto A".to_string(), "Proto B".to_string()]
    );
}

/// V27 (CR 104.4b): loop normalization gives in-flight `EpicCopy` triggers the
/// relation each carrier compared their payloads by, including on a
/// twice-normalized ring prior.
#[test]
fn loop_comparison_of_in_flight_epic_copies_matches_base() {
    let m = |a: &GameState, b: &GameState| loop_states_equal_modulo_resources(a, b);
    let ring = |a: &GameState, b: &GameState| m(&a.normalize_for_loop(), b);
    let distinct_on_stack = |state: &GameState| {
        let mut handles = stack_handles(state);
        handles.sort();
        assert_eq!(
            handles,
            vec![EpicEffectId(0), EpicEffectId(1)],
            "reach-guard: two triggers naming distinct handles"
        );
    };

    // C: the snapshots differ only in their cast coordinates' journal index.
    let (mut runner, _) = stolen_twice(true);
    let turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P1, turn));
    let prompt = runner.state().clone();
    assert!(
        m(&prompt, &prompt.clone()),
        "reach-guard: the ordering prompt is loop-comparable"
    );
    let mut swapped_pending = prompt.clone();
    let groups = &mut swapped_pending
        .pending_trigger_order
        .as_mut()
        .unwrap()
        .groups;
    let handles: Vec<EpicEffectId> = groups
        .iter()
        .flat_map(|group| group.triggers.iter())
        .map(|ctx| handle_of(&ctx.pending.ability))
        .collect();
    for (ctx, handle) in groups
        .iter_mut()
        .flat_map(|group| group.triggers.iter_mut())
        .zip(handles.into_iter().rev())
    {
        ctx.pending.ability.effect = Effect::EpicCopy { epic: handle };
    }
    assert!(
        !m(&prompt, &swapped_pending),
        "trigger carriers compare their payloads raw"
    );
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("the ordering is accepted");
    let c = runner.state().clone();
    distinct_on_stack(&c);
    let swapped = swapped_stack_handles(&c);
    assert!(m(&c, &c.clone()));
    assert!(m(&c, &swapped), "stack carriers compare journal-free");
    assert!(ring(&c, &swapped));
    assert!(loop_states_equal(
        &c.normalize_for_loop(),
        &swapped.normalize_for_loop()
    ));

    // H: the snapshots differ only in trigger identity.
    let mut runner = identity_only_board();
    submit_order(&mut runner);
    let h = runner.state().clone();
    distinct_on_stack(&h);
    let swapped = swapped_stack_handles(&h);
    assert!(m(&h, &h.clone()));
    assert!(!m(&h, &swapped));
    assert!(!ring(&h, &swapped));
    assert!(
        ring(&h, &h.clone()),
        "a twice-normalized prior compares as once"
    );

    // K: the snapshots differ only in a cost record's incarnation.
    let mut runner = cost_record_board();
    submit_order(&mut runner);
    let k = runner.state().clone();
    distinct_on_stack(&k);
    assert!(m(&k, &k.clone()));
    assert!(!m(&k, &swapped_stack_handles(&k)));
}

/// V28: the stack serialization the AI search position hash folds is one byte
/// sequence for the two orders of an equal-snapshot pair, and two for an
/// unequal pair.
#[test]
fn equal_epic_snapshots_serialize_one_stack_in_either_order() {
    let stack_json = |state: &GameState| serde_json::to_value(&state.stack).unwrap();

    let (mut runner, _) = stolen_twice(false);
    let turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P1, turn));
    let b = runner.state().clone();
    assert_eq!(
        stack_handles(&b),
        vec![EpicEffectId(0), EpicEffectId(0)],
        "reach-guard"
    );
    assert_eq!(stack_json(&b), stack_json(&b.clone()));
    assert_eq!(stack_json(&b), stack_json(&other_order(&b)));

    let (mut runner, _) = stolen_twice(true);
    let turn = runner.state().turn_number;
    advance_until(&mut runner, at_next_upkeep_of(P1, turn));
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("the ordering is accepted");
    let c = runner.state().clone();
    let mut handles = stack_handles(&c);
    handles.sort();
    assert_eq!(
        handles,
        vec![EpicEffectId(0), EpicEffectId(1)],
        "reach-guard"
    );
    assert_eq!(stack_json(&c), stack_json(&c.clone()));
    assert_ne!(stack_json(&c), stack_json(&other_order(&c)));
}

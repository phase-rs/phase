//! Tests for Epic (CR 702.50). Declared from `effects/mod.rs` so `epic.rs`
//! stays implementation-only.

use super::epic::{arm_epic, is_epic_locked, resolve};
use crate::game::engine::apply;
use crate::game::stack::resolve_top;
use crate::game::triggers::check_delayed_triggers;
use crate::game::turns::execute_cleanup;
use crate::game::zones::create_object;
use crate::types::ability::{Effect, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
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
            ability: Some(snapshot(id)),
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

    let epic_ability = ResolvedAbility::new(
        Effect::EpicCopy {
            spell: Box::new(snapshot(proto)),
        },
        Vec::new(),
        proto,
        PlayerId(0),
    );

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

    let epic_ability = ResolvedAbility::new(
        Effect::EpicCopy {
            spell: Box::new(targeted_snapshot(proto)),
        },
        Vec::new(),
        proto,
        PlayerId(0),
    );

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

    let epic_ability = ResolvedAbility::new(
        Effect::EpicCopy {
            spell: Box::new(targeted_snapshot(proto)),
        },
        Vec::new(),
        proto,
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve(&mut state, &epic_ability, &mut events).expect("EpicCopy opens retarget choice");

    let result = apply(&mut state, PlayerId(0), GameAction::KeepAllCopyTargets)
        .expect("retarget choice completes");

    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::EpicCopy,
            source_id,
        } if *source_id == proto
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

    state.waiting_for = WaitingFor::CopyRetarget {
        player: PlayerId(0),
        copy_id,
        target_slots: vec![crate::types::game_state::CopyTargetSlot {
            current: Some(crate::types::ability::TargetRef::Player(PlayerId(1))),
            legal_alternatives: Vec::new(),
        }],
        effect_kind: EffectKind::CopySpell,
        effect_source_id: None,
        current_slot: 0,
        paradigm_remaining_offers: None,
    };

    let result = apply(&mut state, PlayerId(0), GameAction::KeepAllCopyTargets)
        .expect("legacy retarget choice completes");

    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::CopySpell,
            source_id,
        } if *source_id == copy_id
    )));
}

#[test]
fn epic_copy_is_a_noop_when_the_prototype_is_gone() {
    // CR 608.2: with no last-known prototype object, no copy can be built.
    let mut state = GameState::new_two_player(42);
    let missing = ObjectId(9999);
    let epic_ability = ResolvedAbility::new(
        Effect::EpicCopy {
            spell: Box::new(snapshot(missing)),
        },
        Vec::new(),
        missing,
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve(&mut state, &epic_ability, &mut events).expect("resolves as a no-op");
    assert!(state.stack.is_empty(), "no copy is created");
}

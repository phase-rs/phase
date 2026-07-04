use crate::game::ability_utils::build_resolved_from_def;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, CopyRetargetPermission,
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, CopyTargetSlot, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// CR 707.10: Copy a spell or ability by putting a copy onto the stack with the
/// same characteristics and choices.
/// CR 707.10c: Some copy effects let the controller choose new targets before
/// the copy is put onto the stack.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 707.10 / CR 702.153a (Casualty): resolve which stack entry to copy.
    // The helper handles explicit object targets (Twincast / Gogo), SelfRef
    // (Casualty triggers whose intermediate stack pushes would make stack.last()
    // wrong), and untargeted fallback (top of stack).
    let top_entry = copy_source_entry(state, ability).ok_or_else(|| {
        EffectError::MissingParam("No spell or ability on stack to copy".to_string())
    })?;

    if stack_entry_cant_be_copied(state, &top_entry) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 707.10: The player under whose control the copy is put on the stack.
    // Twincast/Gogo: the effect's controller. Chain cycle ("that player may
    // copy this spell"): the targeted player. CR 702.144a (Demonstrate): the
    // `copier` override routes the copy to a player relative to the controller.
    let copy_controller = resolve_copy_controller(state, ability);

    let (additional_modifications, starting_loyalty_from_casualty_sacrifice) = match &ability.effect
    {
        Effect::CopySpell {
            additional_modifications,
            starting_loyalty_from_casualty_sacrifice,
            ..
        } => (
            additional_modifications.clone(),
            *starting_loyalty_from_casualty_sacrifice,
        ),
        _ => (Vec::new(), false),
    };

    // Allocate a new stack ID for the copy.
    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // CR 707.10: A spell copy is itself a spell on the stack. Ability stack
    // entries are objects too, but this engine does not store GameObjects for
    // activated/triggered ability entries; clone a GameObject only when the
    // copied stack entry already has one.
    if let Some(source_obj) = state.objects.get(&top_entry.id) {
        let mut copy_obj = source_obj.clone();
        copy_obj.id = copy_id;
        copy_obj.controller = copy_controller;
        copy_obj.zone = Zone::Stack;
        copy_obj.is_token = true;
        copy_obj.additional_cost_payment_count = 0;
        copy_obj.kickers_paid.clear();
        // CR 707.10: A copy of a spell is put on the stack; it is not cast.
        // Inherit no cast-from-zone provenance — otherwise "if this spell was
        // cast from a graveyard" riders (Sevinne's Reclamation, issue #3283)
        // re-fire when a flashback copy resolves.
        copy_obj.cast_from_zone = None;
        apply_spell_copy_modifications(
            &mut copy_obj,
            &additional_modifications,
            starting_loyalty_from_casualty_sacrifice,
            top_entry.ability(),
        );
        state.objects.insert(copy_id, copy_obj);
    }

    // CR 707.10: The copy has the same characteristics as the original, but its
    // identity is distinct.
    //   - Reset additional_cost_paid + kickers_paid so any "if its [additional]
    //     cost was paid" triggers (Offspring ETB, Casualty) do not fire for the
    //     copy — the copy is placed on the stack, not cast.
    //   - Spell copies are new spell objects, so update internal source_id
    //     references throughout the spell ability chain to copy_id. Ability
    //     copies keep the original ability source (CR 707.10b), so their
    //     `SelfRef` effects still refer to the permanent/source that produced
    //     the copied ability.
    //   - Re-controller the resolved ability chain so opponent-controlled copies
    //     (Twincast, Gogo) resolve under the copying player.
    let copy_kind = {
        let mut kind = top_entry.kind.clone();
        match &mut kind {
            StackEntryKind::Spell {
                ability: Some(ref mut a),
                ..
            } => {
                set_resolved_source_recursive(a, copy_id);
                clear_cast_from_zone_recursive(a);
                a.context.additional_cost_paid = false;
                a.context.alternative_mana_cost_paid = false;
                a.context.additional_cost_payment_count = 0;
                a.context.kickers_paid.clear();
            }
            StackEntryKind::Spell { ability: None, .. } => {}
            StackEntryKind::ActivatedAbility { ability, .. } => {
                preserve_ability_copy_source_recursive(ability);
            }
            StackEntryKind::TriggeredAbility { ability, .. } => {
                preserve_ability_copy_source_recursive(ability);
            }
            StackEntryKind::KeywordAction { .. } => {}
        }
        set_copied_kind_controller(&mut kind, copy_controller);
        kind
    };

    // CR 707.10 / CR 707.10b: spell copies source themselves; ability copies
    // have the same source as the original ability.
    let copy_source_id = stack_entry_source_id_for_copy(&copy_kind, copy_id);
    let copy_entry = StackEntry {
        id: copy_id,
        source_id: copy_source_id,
        controller: copy_controller,
        kind: copy_kind,
    };

    // CR 707.10: Capture the copied spell's card id before the entry is moved
    // onto the stack. Only spell copies emit `SpellCopied` — copying an
    // activated/triggered ability is not "copying a spell", so Magecraft and
    // other copy-an-instant-or-sorcery-spell triggers must not see it.
    let copied_spell_card_id = match &copy_entry.kind {
        StackEntryKind::Spell { card_id, .. } => Some(*card_id),
        StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::TriggeredAbility { .. }
        | StackEntryKind::KeywordAction { .. } => None,
    };

    state.stack.push_back(copy_entry);
    events.push(GameEvent::StackPushed { object_id: copy_id });

    // CR 707.10: A copy of a spell is itself a spell on the stack, but it
    // isn't cast. Emit a distinct `SpellCopied` event so copy-sensitive
    // triggers (Magecraft) fire without wrongly firing cast-only triggers.
    if let Some(card_id) = copied_spell_card_id {
        let spell_copied = GameEvent::SpellCopied {
            card_id,
            controller: copy_controller,
            object_id: copy_id,
            original_id: top_entry.id,
        };
        events.push(spell_copied.clone());
        // CR 603.2 + CR 707.10: Magecraft (`SpellCastOrCopy`) and other copy
        // observers must react when a spell copy is created mid-resolution.
        // Collect now; drain after the copy is fully formed (CR 707.10c retarget
        // choice completes, or immediately when no retarget pause is needed).
        crate::game::triggers::collect_triggers_into_deferred(state, &[spell_copied]);
    }

    // CR 707.10c: If the copy has targets, allow the controller to choose new ones.
    let copy_targets = top_entry
        .ability()
        .map(|a| a.targets.clone())
        .unwrap_or_default();

    // CR 707.10c / CR 115.1: arm retarget selection only when the copy effect
    // explicitly granted "you may choose new targets". Otherwise the copy keeps
    // the original spell's declared targets (already present on the cloned
    // stack entry) and resolution proceeds without a player choice.
    if !copy_targets.is_empty()
        && matches!(
            ability.effect,
            Effect::CopySpell {
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                ..
            }
        )
    {
        let Some(copy_ability) = state
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .cloned()
        else {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            drain_spell_copied_observer_triggers(state, events, copied_spell_card_id.is_some())?;
            return Ok(());
        };
        open_copy_retarget_choice(
            state,
            copy_controller,
            copy_id,
            &copy_targets,
            &copy_ability,
            EffectKind::CopySpell,
            copy_id,
        );
        // EffectResolved deferred until after retarget choice completes.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    drain_spell_copied_observer_triggers(state, events, copied_spell_card_id.is_some())?;
    Ok(())
}

/// CR 707.9 + CR 707.2: Stamp copy exceptions onto a spell copy's GameObject at
/// creation (Ob Nixilis: "the copy isn't legendary and has starting loyalty X").
fn apply_spell_copy_modifications(
    copy_obj: &mut crate::game::game_object::GameObject,
    modifications: &[ContinuousModification],
    starting_loyalty_from_casualty_sacrifice: bool,
    source_ability: Option<&ResolvedAbility>,
) {
    for modification in modifications {
        if let ContinuousModification::RemoveSupertype { supertype } = modification {
            copy_obj.card_types.supertypes.retain(|s| s != supertype);
            copy_obj
                .base_card_types
                .supertypes
                .retain(|s| s != supertype);
        }
    }
    if starting_loyalty_from_casualty_sacrifice {
        if let Some(power) = source_ability
            .and_then(|a| a.cost_paid_object.as_ref())
            .and_then(|snap| snap.lki.power)
        {
            let loyalty = power.max(0) as u32;
            // CR 306.5b: seed the entering face's printed loyalty, not live
            // counters — stack objects lose counters at the zone-change boundary
            // (CR 122.2), and ETB reads loyalty from the face values.
            copy_obj.base_loyalty = Some(loyalty);
            copy_obj.loyalty = Some(loyalty);
        }
    }
}

/// CR 603.2 + CR 707.10: Drain `SpellCopied` observers collected when the copy
/// was announced. Deferred until the copy is fully formed — after any CR 707.10c
/// retarget choice, or immediately when no retarget pause is armed.
fn drain_spell_copied_observer_triggers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    spell_copied_collected: bool,
) -> Result<(), EffectError> {
    if spell_copied_collected {
        if let Some(wf) =
            crate::game::triggers::drain_deferred_triggers_after_stack_object_announcement(
                state, events,
            )
        {
            state.waiting_for = wf;
        }
    }
    Ok(())
}

/// CR 707.10c: Open the shared "may choose new targets" choice for a copied
/// spell. The copy is already on the stack; `copy_ability` is the copy's
/// re-sourced ability, so legal alternatives reflect the copy's identity.
pub(crate) fn open_copy_retarget_choice(
    state: &mut GameState,
    copy_controller: PlayerId,
    copy_id: ObjectId,
    copy_targets: &[TargetRef],
    copy_ability: &ResolvedAbility,
    effect_kind: EffectKind,
    effect_source_id: ObjectId,
) {
    // Compute legal alternatives for each slot so the UI can present valid
    // choices. If build_target_slots fails (no legal targets exist for the
    // copy), fall back to empty alternatives — the copy still goes on the
    // stack and will fizzle at resolution per CR 608.2b if all targets remain
    // illegal.
    let selection_slots =
        super::super::ability_utils::build_target_slots(state, copy_ability).unwrap_or_default();

    let target_slots: Vec<CopyTargetSlot> = copy_targets
        .iter()
        .enumerate()
        .map(|(i, t)| CopyTargetSlot {
            current: Some(t.clone()),
            legal_alternatives: selection_slots
                .get(i)
                .map(|s| s.legal_targets.clone())
                .unwrap_or_default(),
        })
        .collect();

    // CR 707.10c: "its controller may choose new targets for the copy" — the
    // copy's controller makes the retarget choice.
    state.waiting_for = WaitingFor::CopyRetarget {
        player: copy_controller,
        copy_id,
        target_slots,
        effect_kind,
        effect_source_id: Some(effect_source_id),
        current_slot: 0,
        paradigm_remaining_offers: None,
    };
}

/// CR 707.10: "A copy of a spell is controlled by the player under whose
/// control it was put on the stack." For most copy effects (Twincast, Gogo)
/// that is the effect's controller — the player resolving the copy spell. But
/// for "that player may copy this spell" effects (the Chain cycle — Chain of
/// Acid / Plasma / Smog / Vapor), the copy is created by, and controlled by,
/// the *targeted* player, not the original spell's caster.
///
/// The targeted player arrives as a `TargetRef::Player` in `ability.targets`:
/// Chain of Smog's `CopySpell` sub-ability inherits the parent `Discard`'s
/// `[TargetRef::Player]` target during chain resolution. A copy effect targets
/// either a spell/ability to copy (`TargetRef::Object`) or — for the
/// player-anchored Chain pattern — the copying player; the two are disjoint, so
/// a `TargetRef::Player` in scope unambiguously identifies the copy's
/// controller.
fn copy_controller(ability: &ResolvedAbility) -> PlayerId {
    ability
        .targets
        .iter()
        .find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            TargetRef::Object(_) => None,
        })
        .unwrap_or(ability.controller)
}

/// CR 707.10: Determine the player who puts the copy onto the stack (and thus
/// controls it). An explicit `TargetRef::Player` (the Chain cycle's inherited
/// player target) always wins. Otherwise an `Effect::CopySpell { copier:
/// Some(ref), .. }` override resolves a player relative to the controller — CR
/// 702.144a (Demonstrate) sets `copier: Opponent` so a chosen opponent copies.
/// With no target and no copier, the effect's controller copies
/// (Twincast/Casualty/Replicate).
fn resolve_copy_controller(state: &GameState, ability: &ResolvedAbility) -> PlayerId {
    if let Effect::CopySpell {
        copier: Some(cref), ..
    } = &ability.effect
    {
        // A declared player target (Chain cycle) takes precedence over the
        // copier override; otherwise resolve the override to a concrete player.
        let has_player_target = ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Player(_)));
        if !has_player_target {
            if let Some(player) = resolve_copier_player(state, cref, ability.controller) {
                return player;
            }
        }
    }
    copy_controller(ability)
}

/// CR 702.144a + CR 102.2 / CR 102.3: Resolve a `copier` `ControllerRef` to a
/// concrete player relative to the copy's controller. Only the variants
/// meaningful as a copier are handled: `You` (the controller) and `Opponent`
/// (the controller's opponent). Other refs return `None`, so the caller falls
/// back to the default controller. NOTE: with multiple opponents, "choose an
/// opponent" (CR 702.144a) resolves to the first opponent in turn order; an
/// interactive multiplayer choice is left as a follow-up. In two-player games
/// (the common case) this is exact, since there is a single opponent.
fn resolve_copier_player(
    state: &GameState,
    cref: &ControllerRef,
    controller: PlayerId,
) -> Option<PlayerId> {
    match cref {
        ControllerRef::You => Some(controller),
        ControllerRef::Opponent => crate::game::players::opponents(state, controller)
            .into_iter()
            .next(),
        ControllerRef::ScopedPlayer
        | ControllerRef::TargetPlayer
        | ControllerRef::ParentTargetController
        | ControllerRef::ParentTargetOwner
        | ControllerRef::DefendingPlayer
        | ControllerRef::ChosenPlayer { .. }
        | ControllerRef::SourceChosenPlayer
        | ControllerRef::TriggeringPlayer
        // CR 303.4b: Enchanted-player scope cannot resolve a copier. Fail closed.
        | ControllerRef::EnchantedPlayer => None,
    }
}

/// CR 707.10 + CR 614.1a: Apply active "copy an additional time" replacement
/// effects (Twinning Staff) to the number of copies a `CopySpell` effect would
/// create. `base` is the count the effect would otherwise produce (its
/// `repeat_for` value, or 1); the return value is the modified count.
///
/// Copies are produced by the generic `repeat_for` loop, not the
/// `ProposedEvent` replacement pipeline, so the count modification is applied
/// here at the copy-count site. Only copies of a *spell* are affected — copying
/// an activated/triggered ability (Gogo) is not "copying a spell" (CR 707.10).
/// Each `CopySpell` replacement controlled by the copy's controller folds its
/// `QuantityModification` into the count; purely additive `Plus` modifications
/// (the only shape in the current card pool) are order-independent, so no
/// CR 616.1 ordering choice is required.
pub(crate) fn copy_count_with_replacements(
    state: &GameState,
    ability: &ResolvedAbility,
    base: usize,
) -> usize {
    use crate::types::ability::QuantityModification;
    use crate::types::replacements::ReplacementEvent;

    // CR 614.1: "If you would copy a spell *one or more times*" — a replacement
    // effect watches for a particular event that *would happen*. When the effect
    // would make zero copies (e.g. a "copy for each X" with X = 0) there is no
    // copy event to watch for, so the bonus must not apply.
    if base == 0 {
        return 0;
    }

    // CR 707.10: Twinning Staff only modifies copying a *spell*, not an ability.
    match copy_source_entry(state, ability) {
        Some(entry) if matches!(entry.kind, StackEntryKind::Spell { .. }) => {}
        _ => return base,
    }

    // CR 707.10 / CR 614.1a: "if you would copy" — only the copy controller's
    // copy-additional replacements apply.
    let controller = copy_controller(ability);
    // Keep `count` as `usize` and widen the `u32` modification values into it.
    // Widening (`value as usize`) is always lossless; the earlier `base as u32`
    // was a narrowing cast that could truncate on 64-bit targets.
    let mut count = base;
    for (_idx, obj, def) in crate::game::functioning_abilities::active_replacements(state) {
        let source_functions =
            obj.zone == Zone::Battlefield || (obj.zone == Zone::Command && obj.is_emblem);
        if def.event != ReplacementEvent::CopySpell
            || obj.controller != controller
            || !source_functions
        {
            continue;
        }
        count = match def.quantity_modification {
            Some(QuantityModification::Times { factor }) => count.saturating_mul(factor as usize),
            Some(QuantityModification::Half) => count / 2,
            Some(QuantityModification::Plus { value }) => count.saturating_add(value as usize),
            Some(QuantityModification::Minus { value }) => count.saturating_sub(value as usize),
            // `Prevent` / unspecified is not a copy-count increase — leave as-is.
            Some(QuantityModification::Prevent) | None => count,
        };
    }
    count
}

fn copy_source_entry(state: &GameState, ability: &ResolvedAbility) -> Option<StackEntry> {
    let target_id = ability.targets.iter().find_map(|target| match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    });
    if let Some(target_id) = target_id {
        return state
            .stack
            .iter()
            .rev()
            .find(|entry| {
                entry.id == target_id
                    || entry.source_id == target_id
                    || matches!(
                        &entry.kind,
                        StackEntryKind::ActivatedAbility {
                            source_id: activated_id,
                            ..
                        } if *activated_id == target_id
                    )
            })
            .cloned();
    }
    if let Effect::CopySpell { target, .. } = &ability.effect {
        if references_tracked_set(target) {
            return copy_source_from_tracked_set(state, ability, target);
        }
    }
    if matches!(
        &ability.effect,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            ..
        }
    ) {
        // The source spell is normally still on the stack — Casualty's copy
        // trigger resolves while the spell waits beneath it.
        if let Some(entry) = state
            .stack
            .iter()
            .find(|entry| entry.id == ability.source_id)
            .cloned()
        {
            return Some(entry);
        }
        // CR 707.10: When the `CopySpell` is the resolving spell's OWN effect
        // (the Chain cycle — "you may copy this spell"), `resolve_top` has
        // already popped that spell off the stack. Fall back to the resolving
        // stack entry stashed by `resolve_top` so the spell can still copy
        // itself.
        if let Some(entry) = state.resolving_stack_entry.as_ref() {
            if entry.id == ability.source_id {
                return Some(entry.clone());
            }
        }
        return None;
    }
    if let Some(entry) = triggering_spell_stack_entry(state) {
        return Some(entry);
    }
    state.stack.last().cloned()
}

fn references_tracked_set(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => true,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(references_tracked_set)
        }
        TargetFilter::Not { filter } => references_tracked_set(filter),
        _ => false,
    }
}

fn tracked_set_id_from_filter(filter: &TargetFilter) -> Option<TrackedSetId> {
    match filter {
        TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => Some(*id),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().find_map(tracked_set_id_from_filter)
        }
        TargetFilter::Not { filter } => tracked_set_id_from_filter(filter),
        _ => None,
    }
}

/// CR 707.10 + CR 702.153a (Isochron Scepter): `CopySpell { TrackedSet }` copies
/// an imprinted card from exile, not the top of the stack.
fn copy_source_from_tracked_set(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Option<StackEntry> {
    if !references_tracked_set(target) {
        return None;
    }
    let effective_filter =
        crate::game::targeting::resolve_tracked_set_sentinel(state, target.clone());
    let tracked_set_id = tracked_set_id_from_filter(&effective_filter)
        .or_else(|| crate::game::targeting::latest_tracked_set_id(state))
        .or(state.chain_tracked_set_id)?;
    let ctx = FilterContext::from_ability(ability);
    let source_id = state
        .tracked_object_sets
        .get(&tracked_set_id)?
        .iter()
        .copied()
        .find(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Exile
                    && matches_target_filter(state, *id, &effective_filter, &ctx)
            })
        })?;
    stack_entry_from_exiled_spell_object(state, source_id, ability.controller)
}

fn stack_entry_from_exiled_spell_object(
    state: &GameState,
    object_id: ObjectId,
    controller: PlayerId,
) -> Option<StackEntry> {
    let obj = state.objects.get(&object_id)?;
    if obj.zone != Zone::Exile {
        return None;
    }
    let card_id = obj.card_id;
    let ability_def = spell_ability_definition(&obj.abilities)?;
    let resolved = build_resolved_from_def(&ability_def, object_id, controller);
    Some(StackEntry {
        id: object_id,
        source_id: object_id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(resolved),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    })
}

fn spell_ability_definition(abilities: &[AbilityDefinition]) -> Option<AbilityDefinition> {
    abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Spell)
        .cloned()
}

/// CR 601.2i + CR 603.2 + CR 707.10: "Copy that spell" / `Trigger_ThatSpell`
/// on a `SpellCast` trigger (Mendicant Core, Guidelight; Twincast-style copies
/// gated on optional costs). When another triggered ability sits above the cast
/// spell on the stack (Rhystic Study, Monastery Mentor, etc.), `stack.last()`
/// points at the wrong entry — bind from the triggering event's spell instead.
fn triggering_spell_stack_entry(state: &GameState) -> Option<StackEntry> {
    let event = state.current_trigger_event.as_ref()?;
    let object_id = crate::game::targeting::extract_source_from_event(event)?;
    if matches!(event, GameEvent::AbilityActivated { .. }) {
        if let Some(entry) = state.stack.iter().rev().find(|entry| {
            matches!(
                entry.kind,
                StackEntryKind::ActivatedAbility {
                    source_id: activated_id,
                    ..
                } if activated_id == object_id
            ) || entry.source_id == object_id
        }) {
            return Some(entry.clone());
        }
    }
    let mut fallback = None;
    for entry in state.stack.iter().rev() {
        if entry.id == object_id {
            return Some(entry.clone());
        }
        if fallback.is_none() && entry.source_id == object_id {
            fallback = Some(entry.clone());
        }
    }
    fallback
}

fn stack_entry_cant_be_copied(state: &GameState, entry: &StackEntry) -> bool {
    if entry
        .ability()
        .is_some_and(|ability| ability.cant_be_copied)
    {
        return true;
    }

    state
        .objects
        .get(&entry.id)
        .map(|obj| {
            super::super::functioning_abilities::active_static_definitions(state, obj)
                .any(|sd| sd.mode == StaticMode::CantBeCopied)
        })
        .unwrap_or(false)
}

fn set_copied_kind_controller(kind: &mut StackEntryKind, controller: PlayerId) {
    match kind {
        StackEntryKind::Spell {
            ability: Some(ability),
            ..
        }
        | StackEntryKind::ActivatedAbility { ability, .. } => {
            set_resolved_controller_recursive(ability, controller);
        }
        StackEntryKind::TriggeredAbility { ability, .. } => {
            set_resolved_controller_recursive(ability, controller);
        }
        StackEntryKind::Spell { ability: None, .. } | StackEntryKind::KeywordAction { .. } => {}
    }
}

fn set_resolved_controller_recursive(ability: &mut ResolvedAbility, controller: PlayerId) {
    ability.controller = controller;
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        set_resolved_controller_recursive(sub_ability, controller);
    }
    if let Some(else_ability) = ability.else_ability.as_mut() {
        set_resolved_controller_recursive(else_ability, controller);
    }
}

/// CR 707.10b: A copy is a new object; rewrite `source_id` on every link of
/// the resolved ability chain (the top-level effect plus its `sub_ability` /
/// `else_ability` descendants) so a `SelfRef` anywhere in the chain resolves
/// to the copy. Mirrors `set_resolved_controller_recursive`. Without this, the
/// Chain cycle's nested optional `CopySpell` would keep the original spell's
/// `source_id` and a second-generation copy could not find its source.
pub(crate) fn set_resolved_source_recursive(ability: &mut ResolvedAbility, source_id: ObjectId) {
    ability.source_id = source_id;
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        set_resolved_source_recursive(sub_ability, source_id);
    }
    if let Some(else_ability) = ability.else_ability.as_mut() {
        set_resolved_source_recursive(else_ability, source_id);
    }
}

fn preserve_ability_copy_source_recursive(ability: &mut ResolvedAbility) {
    let source_id = ability.source_id;
    set_resolved_source_recursive(ability, source_id);
}

fn stack_entry_source_id_for_copy(kind: &StackEntryKind, copy_id: ObjectId) -> ObjectId {
    match kind {
        StackEntryKind::Spell { .. } | StackEntryKind::KeywordAction { .. } => copy_id,
        StackEntryKind::ActivatedAbility { source_id, .. }
        | StackEntryKind::TriggeredAbility { source_id, .. } => *source_id,
    }
}

/// CR 707.10: Spell copies are not cast, so strip cast-origin metadata from
/// the copied ability chain before the copy resolves.
fn clear_cast_from_zone_recursive(ability: &mut ResolvedAbility) {
    ability.context.cast_from_zone = None;
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        clear_cast_from_zone_recursive(sub_ability);
    }
    if let Some(else_ability) = ability.else_ability.as_mut() {
        clear_cast_from_zone_recursive(else_ability);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{
        ControllerRef, CopyRetargetPermission, Effect, EffectScope, QuantityExpr, QuantityRef,
        TapStateChange, TargetFilter, TargetRef,
    };
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::player::PlayerId;

    /// Helper: push a spell onto the stack with a matching GameObject.
    fn push_spell(
        state: &mut GameState,
        obj_id: ObjectId,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        ability: ResolvedAbility,
        variant: CastingVariant,
    ) {
        let obj = GameObject::new(obj_id, card_id, owner, name.to_string(), Zone::Stack);
        state.objects.insert(obj_id, obj);
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: owner,
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(ability),
                casting_variant: variant,
                actual_mana_spent: 0,
            },
        });
    }

    #[test]
    fn test_copy_spell_duplicates_stack_entry() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability.clone(),
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // Stack should have 2 entries now
        assert_eq!(state.stack.len(), 2);
        // Copy should have a different ID
        assert_ne!(state.stack[0].id, state.stack[1].id);

        // Engine bookkeeping: spell copies get a stack GameObject.
        let copy_id = state.stack[1].id;
        let copy_obj = state.objects.get(&copy_id).expect("copy object exists");
        assert!(copy_obj.is_token);
        assert_eq!(copy_obj.zone, Zone::Stack);

        // Same spell kind
        match (&state.stack[0].kind, &state.stack[1].kind) {
            (
                StackEntryKind::Spell {
                    card_id: c1,
                    ability: Some(a1),
                    ..
                },
                StackEntryKind::Spell {
                    card_id: c2,
                    ability: Some(a2),
                    ..
                },
            ) => {
                assert_eq!(c1, c2);
                assert_eq!(
                    crate::types::ability::effect_variant_name(&a1.effect),
                    crate::types::ability::effect_variant_name(&a2.effect)
                );
            }
            _ => panic!("Expected both entries to be Spells with abilities"),
        }
    }

    /// CR 702.144a + CR 707.10: a `CopySpell { copier: Some(Opponent) }` puts the
    /// copy onto the stack under an OPPONENT's control (Demonstrate's
    /// opponent-copy). In a two-player game the single opponent is chosen
    /// deterministically. Revert-discriminating: before the `copier` field the
    /// copy was always controlled by the effect's controller.
    #[test]
    fn copy_spell_copier_opponent_routes_copy_to_opponent() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );

        // The copy effect is controlled by P0, but `copier: Opponent` means P1
        // puts the copy onto the stack and controls it.
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: Some(ControllerRef::Opponent),
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();

        assert_eq!(state.stack.len(), 2, "the copy was added to the stack");
        let copy_id = state.stack[1].id;
        let copy_obj = state.objects.get(&copy_id).expect("copy object exists");
        assert_eq!(
            copy_obj.controller,
            PlayerId(1),
            "CR 702.144a: the chosen opponent controls the Demonstrate copy"
        );
        // The copy's resolved ability chain is re-controllered to the opponent.
        if let StackEntryKind::Spell {
            ability: Some(a), ..
        } = &state.stack[1].kind
        {
            assert_eq!(a.controller, PlayerId(1));
        } else {
            panic!("copy should be a Spell with an ability");
        }
    }

    /// CR 702.144a + CR 608.2c: Demonstrate's opponent copy is conditional on
    /// the controller accepting the optional self-copy. A declined optional
    /// self-copy must not run the opponent sub-copy.
    #[test]
    fn demonstrate_decline_skips_opponent_subcopy() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Creative Technique",
            original_ability,
            CastingVariant::Normal,
        );

        let opponent_copy = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: Some(ControllerRef::Opponent),
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        let mut demonstrate = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        )
        .sub_ability(opponent_copy);
        demonstrate.optional = true;

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &demonstrate, &mut events, 0)
            .unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Demonstrate self-copy should pause for the optional choice"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        assert_eq!(
            state.stack.len(),
            1,
            "declining Demonstrate must not put either copy onto the stack"
        );
        assert!(
            state.pending_continuation.is_none(),
            "declining Demonstrate must not leave the opponent copy queued"
        );
        assert!(
            state
                .objects
                .values()
                .all(|obj| !obj.is_token || obj.zone != Zone::Stack),
            "declining Demonstrate must not create a spell-copy token"
        );
    }

    /// CR 707.10: `copier: None` (the default for Twincast/Casualty/Replicate)
    /// keeps the copy under the effect controller's control — the new field is
    /// inert unless explicitly set.
    #[test]
    fn copy_spell_copier_none_keeps_controller() {
        let mut state = GameState::new_two_player(42);
        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();

        let copy_id = state.stack[1].id;
        let copy_obj = state.objects.get(&copy_id).expect("copy object exists");
        assert_eq!(copy_obj.controller, PlayerId(0));
    }

    /// CR 707.10: an explicit `TargetRef::Player` (the Chain cycle's inherited
    /// player target) takes precedence over a `copier` override — guards the
    /// `has_player_target` short-circuit in `resolve_copy_controller`.
    #[test]
    fn copy_spell_explicit_player_target_beats_copier() {
        let mut state = GameState::new_two_player(42);
        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );
        // copier says "You" (P0), but an explicit player target (P1) must win.
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: Some(ControllerRef::You),
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();
        let copy_id = state.stack[1].id;
        assert_eq!(
            state.objects.get(&copy_id).unwrap().controller,
            PlayerId(1),
            "an explicit player target must control the copy over the copier override"
        );
    }

    /// CR 707.10: `copier: Some(You)` resolves to the effect controller.
    #[test]
    fn copy_spell_copier_you_resolves_to_controller() {
        let mut state = GameState::new_two_player(42);
        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: Some(ControllerRef::You),
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();
        let copy_id = state.stack[1].id;
        assert_eq!(state.objects.get(&copy_id).unwrap().controller, PlayerId(0));
    }

    /// Back-compat: the new `copier` field is `#[serde(default, skip_serializing_if
    /// = "Option::is_none")]`, so `None` is omitted from serialized card data
    /// (older JSON without the field still loads) and a set copier round-trips.
    #[test]
    fn copy_spell_copier_serde_default_and_roundtrip() {
        let none = Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        };
        let json_none = serde_json::to_string(&none).unwrap();
        assert!(
            !json_none.contains("copier"),
            "copier: None must be skipped so existing serialized data is unchanged"
        );
        // Old data (no `copier` key) deserializes back to `None`.
        assert_eq!(serde_json::from_str::<Effect>(&json_none).unwrap(), none);

        let with = Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            copier: Some(ControllerRef::Opponent),
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        };
        let json = serde_json::to_string(&with).unwrap();
        assert!(json.contains("copier"));
        assert_eq!(serde_json::from_str::<Effect>(&json).unwrap(), with);
    }

    #[test]
    fn copy_spell_resets_additional_cost_payment_history() {
        let mut state = GameState::new_two_player(42);

        let mut original_ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::AdditionalCostPaymentCount,
                },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        original_ability.context.additional_cost_paid = true;
        original_ability.context.additional_cost_payment_count = 2;
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Endless Foot Assault",
            original_ability,
            CastingVariant::Normal,
        );
        {
            let obj = state.objects.get_mut(&ObjectId(10)).unwrap();
            obj.additional_cost_payment_count = 2;
        }

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        let copy_id = state.stack.back().expect("copy on stack").id;
        assert_eq!(
            state.objects[&copy_id].additional_cost_payment_count, 0,
            "a spell copy was not cast, so it must not retain Squad payment history"
        );
        let copy_context = state.stack.back().and_then(StackEntry::ability).unwrap();
        assert!(!copy_context.context.additional_cost_paid);
        assert_eq!(copy_context.context.additional_cost_payment_count, 0);
    }

    #[test]
    fn test_copy_spell_empty_stack_returns_error() {
        let mut state = GameState::new_two_player(42);
        assert!(state.stack.is_empty());

        let ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn test_copy_spell_with_targets_enters_retarget() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(ObjectId(50))],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // CR 707.10c: Copy has targets → should enter CopyRetarget.
        assert!(matches!(state.waiting_for, WaitingFor::CopyRetarget { .. }));
        // Copy should still be on the stack
        assert_eq!(state.stack.len(), 2);
    }

    /// CR 115.1 / CR 707.10c: a copy effect WITHOUT the "you may choose new
    /// targets" clause keeps the original spell's targets — even though the
    /// copied spell has targets, no `CopyRetarget` choice is armed.
    #[test]
    fn test_copy_spell_keep_targets_skips_retarget_despite_targets() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(ObjectId(50))],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // KeepOriginalTargets → no retarget choice, resolution completes.
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::CopyRetarget { .. }
        ));
        assert_eq!(state.stack.len(), 2);
        // The copy retains the original's declared target.
        let copy_entry = state.stack.back().unwrap();
        assert_eq!(
            copy_entry.ability().map(|a| a.targets.as_slice()),
            Some([TargetRef::Object(ObjectId(50))].as_slice())
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn test_copy_spell_without_targets_skips_retarget() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            original_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // No targets → should NOT enter CopyRetarget, should emit EffectResolved
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::CopyRetarget { .. }
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    /// Helper: push a triggered ability onto the stack (no targets).
    fn push_trigger(
        state: &mut GameState,
        obj_id: ObjectId,
        card_id: CardId,
        owner: PlayerId,
        ability: ResolvedAbility,
    ) {
        push_trigger_with_event(state, obj_id, card_id, owner, ability, None);
    }

    /// Helper: push a triggered ability onto the stack with an optional trigger event.
    fn push_trigger_with_event(
        state: &mut GameState,
        obj_id: ObjectId,
        card_id: CardId,
        owner: PlayerId,
        ability: ResolvedAbility,
        trigger_event: Option<GameEvent>,
    ) {
        let obj = crate::game::game_object::GameObject::new(
            obj_id,
            card_id,
            owner,
            "Trigger Token".to_string(),
            Zone::Stack,
        );
        state.objects.insert(obj_id, obj);
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: owner,
            kind: StackEntryKind::TriggeredAbility {
                source_id: obj_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });
    }

    /// CR 702.153a (Casualty): When another trigger sits between the original
    /// spell and the Casualty copy trigger, SelfRef lookup must find the spell
    /// by source_id rather than using stack.last().
    #[test]
    fn test_copy_spell_selfref_finds_spell_past_intermediate_trigger() {
        let mut state = GameState::new_two_player(42);

        // Push original targeted spell (Anguished Unmaking-style)
        let original_ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: crate::types::zones::Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(ObjectId(99))],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Anguished Unmaking",
            original_ability.clone(),
            CastingVariant::Normal,
        );

        // Push an intermediate triggered ability (e.g. Monastery Mentor token trigger)
        let mentor_ability = ResolvedAbility::new(
            Effect::Token {
                name: "Monk".to_string(),
                power: crate::types::ability::PtValue::Fixed(1),
                toughness: crate::types::ability::PtValue::Fixed(1),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(11),
            PlayerId(0),
        );
        push_trigger(
            &mut state,
            ObjectId(11),
            CardId(2),
            PlayerId(0),
            mentor_ability,
        );

        // Simulate resolve_top popping the Casualty copy trigger (top of stack).
        // The Casualty ability has source_id = 10 (Anguished Unmaking) and SelfRef target.
        let casualty_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(10), // source_id = original spell
            PlayerId(0),
        );
        let mut events = Vec::new();

        // Stack is now: [Anguished Unmaking (10), Mentor trigger (11)]
        // copy_spell::resolve should find ObjectId(10) via source_id, not stack.last() (=11)
        resolve(&mut state, &casualty_ability, &mut events).unwrap();

        // Should have entered CopyRetarget (original had targets) with the copy of the spell
        assert!(
            matches!(state.waiting_for, WaitingFor::CopyRetarget { .. }),
            "Expected CopyRetarget but got {:?}",
            state.waiting_for
        );
        // Stack: original + mentor trigger + copy = 3 entries
        assert_eq!(state.stack.len(), 3);
        // The copy should be a copy of Anguished Unmaking (ChangeZone), not the Mentor trigger
        let copy_entry = state.stack.back().unwrap();
        assert!(
            copy_entry
                .ability()
                .is_some_and(|a| matches!(a.effect, Effect::ChangeZone { .. })),
            "Copy should replicate ChangeZone (Anguished Unmaking), not the trigger"
        );
    }

    /// CR 601.2i + CR 603.2 + CR 707.10 (issue #1672): Mendicant Core,
    /// Guidelight — copy the triggering artifact spell even when another
    /// triggered ability sits above it on the stack (Rhystic Study class).
    #[test]
    fn copy_spell_triggering_spell_finds_cast_spell_past_intermediate_trigger() {
        let mut state = GameState::new_two_player(42);
        let cast_spell_id = ObjectId(10);
        let rhystic_trigger_id = ObjectId(11);

        let cast_ability = ResolvedAbility::new(
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            cast_spell_id,
            PlayerId(0),
        );
        push_spell(
            &mut state,
            cast_spell_id,
            CardId(1),
            PlayerId(0),
            "Rhystic Study",
            cast_ability,
            CastingVariant::Normal,
        );

        let rhystic_ability = ResolvedAbility::new(
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            rhystic_trigger_id,
            PlayerId(1),
        );
        push_trigger(
            &mut state,
            rhystic_trigger_id,
            CardId(2),
            PlayerId(1),
            rhystic_ability,
        );

        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(1),
            object_id: cast_spell_id,
            controller: PlayerId(0),
        });

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::ParentTarget,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        assert_eq!(
            state.stack.len(),
            3,
            "cast spell, rhystic trigger, and copy"
        );
        let copy_entry = state.stack.back().unwrap();
        assert!(
            copy_entry
                .ability()
                .is_some_and(|a| matches!(a.effect, Effect::Draw { .. })),
            "copy should replicate the cast spell, not the Rhystic trigger on top"
        );
    }

    /// CR 601.2i + CR 603.2 + CR 707.10: the real stack resolver sets
    /// `current_trigger_event` from the triggered stack entry before executing
    /// the copy effect, so the copy source must still be the triggering spell
    /// rather than the intervening topmost trigger.
    #[test]
    fn copy_spell_triggering_spell_uses_stack_entry_trigger_event() {
        let mut state = GameState::new_two_player(42);
        let cast_spell_id = ObjectId(10);
        let rhystic_trigger_id = ObjectId(11);
        let copy_trigger_id = ObjectId(12);

        let cast_ability = ResolvedAbility::new(
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            cast_spell_id,
            PlayerId(0),
        );
        push_spell(
            &mut state,
            cast_spell_id,
            CardId(1),
            PlayerId(0),
            "Mendicant artifact spell",
            cast_ability,
            CastingVariant::Normal,
        );

        let rhystic_ability = ResolvedAbility::new(
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            rhystic_trigger_id,
            PlayerId(1),
        );
        push_trigger(
            &mut state,
            rhystic_trigger_id,
            CardId(2),
            PlayerId(1),
            rhystic_ability,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        push_trigger_with_event(
            &mut state,
            copy_trigger_id,
            CardId(3),
            PlayerId(0),
            copy_ability,
            Some(GameEvent::SpellCast {
                card_id: CardId(1),
                object_id: cast_spell_id,
                controller: PlayerId(0),
            }),
        );

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.stack.len(),
            3,
            "cast spell, rhystic trigger, and copy"
        );
        let copy_entry = state.stack.back().unwrap();
        assert!(
            copy_entry
                .ability()
                .is_some_and(|a| matches!(a.effect, Effect::Draw { .. })),
            "copy should replicate the cast spell, not the Rhystic trigger on top"
        );
        assert!(state.current_trigger_event.is_none());
    }

    /// CR 707.10: "A copy of a spell is controlled by the player under whose
    /// control it was put on the stack." When a `CopySpell` effect carries a
    /// `TargetRef::Player` (the Chain cycle's "That player may copy this
    /// spell"), the copy is controlled by that targeted player — not the
    /// effect's own controller.
    #[test]
    fn copy_spell_with_player_target_is_controlled_by_targeted_player() {
        let mut state = GameState::new_two_player(42);

        // Original spell cast by P0.
        let original_ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Chain of Smog",
            original_ability,
            CastingVariant::Normal,
        );

        // The `CopySpell` sub-ability: controller is the caster (P0), but the
        // parent's `TargetRef::Player(P1)` has been propagated onto it.
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(10),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // CR 707.10: the copy is controlled by the targeted player (P1).
        assert_eq!(state.stack.len(), 2);
        let copy_entry = state.stack.back().unwrap();
        assert_eq!(
            copy_entry.controller,
            PlayerId(1),
            "the copy must be controlled by the targeted player, not the caster"
        );
        let copy_obj = state
            .objects
            .get(&copy_entry.id)
            .expect("the copy has a stack GameObject");
        assert_eq!(copy_obj.controller, PlayerId(1));
        // The cloned resolved ability chain is re-controllered to P1 too.
        assert_eq!(
            copy_entry.ability().map(|a| a.controller),
            Some(PlayerId(1)),
        );
    }

    /// CR 707.10: a `CopySpell` with an `Object` target (Twincast / Gogo —
    /// "copy target spell") is controlled by the effect's own controller; no
    /// `TargetRef::Player` is in scope, so the caster keeps control.
    #[test]
    fn copy_spell_with_object_target_is_controlled_by_effect_controller() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(1),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(1),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );

        // Twincast cast by P0 targeting P1's Bolt on the stack.
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![TargetRef::Object(ObjectId(10))],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        let copy_entry = state.stack.back().unwrap();
        assert_eq!(
            copy_entry.controller,
            PlayerId(0),
            "a copy of a targeted spell is controlled by the copier (P0)"
        );
    }

    #[test]
    fn copy_spell_triggering_source_copies_activated_ability_on_stack() {
        let mut state = GameState::new_two_player(42);
        let source_creature = ObjectId(10);
        let magus = ObjectId(20);

        let mut draw_resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_creature,
            PlayerId(0),
        );
        draw_resolved.chosen_x = Some(2);

        state.stack.push_back(StackEntry {
            id: ObjectId(100),
            source_id: source_creature,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source_creature,
                ability: draw_resolved,
            },
        });

        state.current_trigger_event = Some(GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id: source_creature,
            kind: crate::types::events::ActivatedAbilityKind::Normal,
        });

        let copy_effect = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            magus,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &copy_effect, &mut events).unwrap();

        assert_eq!(
            state.stack.len(),
            2,
            "copy must remain on stack below the copy"
        );
        let copied = state.stack.back().unwrap().ability().expect("copy entry");
        assert_eq!(
            copied.chosen_x,
            Some(2),
            "activated-ability copies must preserve announced X"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::StackPushed { .. })),
            "copying an activated ability must push a stack entry"
        );
    }

    #[test]
    fn copied_activated_ability_keeps_original_source_for_self_ref_resolution() {
        let mut state = GameState::new_two_player(42);
        let basalt = ObjectId(10);
        let rings = ObjectId(20);
        state.objects.insert(
            basalt,
            GameObject::new(
                basalt,
                CardId(10),
                PlayerId(0),
                "Basalt Monolith".to_string(),
                Zone::Battlefield,
            ),
        );
        state.objects.get_mut(&basalt).unwrap().tapped = true;

        let untap_basalt = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![],
            basalt,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: ObjectId(100),
            source_id: basalt,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: basalt,
                ability: untap_basalt,
            },
        });
        state.current_trigger_event = Some(GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id: basalt,
            kind: crate::types::events::ActivatedAbilityKind::Normal,
        });

        let copy_effect = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            rings,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &copy_effect, &mut events).unwrap();

        let copy_entry = state.stack.back().expect("copy entry");
        assert_eq!(
            copy_entry.source_id, basalt,
            "CR 707.10b: copied activated abilities keep the original source"
        );
        assert_eq!(
            copy_entry.ability().map(|ability| ability.source_id),
            Some(basalt),
            "SelfRef on the copied ability must still refer to Basalt Monolith"
        );

        crate::game::stack::resolve_top(&mut state, &mut events);

        assert!(
            !state.objects.get(&basalt).unwrap().tapped,
            "the copied untap ability must untap Basalt Monolith"
        );
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::PermanentUntapped { object_id } if *object_id == basalt)
        ));
    }

    #[test]
    fn copy_spell_hydrated_triggering_source_finds_activated_ability_by_permanent_id() {
        let mut state = GameState::new_two_player(42);
        let source_creature = ObjectId(10);
        let magus = ObjectId(20);

        let mut draw_resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_creature,
            PlayerId(0),
        );
        draw_resolved.chosen_x = Some(2);

        state.stack.push_back(StackEntry {
            id: ObjectId(100),
            source_id: source_creature,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source_creature,
                ability: draw_resolved,
            },
        });

        state.current_trigger_event = Some(GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id: source_creature,
            kind: crate::types::events::ActivatedAbilityKind::Normal,
        });

        let copy_effect = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![TargetRef::Object(source_creature)],
            magus,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &copy_effect, &mut events).unwrap();

        assert_eq!(state.stack.len(), 2);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::StackPushed { .. })),
            "hydrated TriggeringSource must copy the activated ability on stack"
        );
    }

    #[test]
    fn uncopyable_activated_ability_on_stack_is_not_copied_through_stack_resolution() {
        let mut state = GameState::new_two_player(42);
        let gogo_id = ObjectId(20);
        let other_id = ObjectId(21);

        let mut gogo_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::StackAbility {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    tag: None,
                    kind: None,
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            gogo_id,
            PlayerId(0),
        );
        gogo_ability.cant_be_copied = true;

        state.stack.push_back(StackEntry {
            id: ObjectId(40),
            source_id: gogo_id,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: gogo_id,
                ability: gogo_ability,
            },
        });

        let copy_gogo = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::StackAbility {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    tag: None,
                    kind: None,
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![TargetRef::Object(ObjectId(40))],
            other_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: ObjectId(41),
            source_id: other_id,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: other_id,
                ability: copy_gogo,
            },
        });

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.stack[0].id, ObjectId(40));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::StackPushed { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
    }

    /// CR 707.10 + CR 207.2c (Magecraft ability word): copying an instant or
    /// sorcery spell fires a `TriggerMode::SpellCastOrCopy` trigger (Magecraft).
    /// Pipeline test: drives the real `copy_spell::resolve` → `process_triggers`
    /// path, not a synthetic `GameEvent`. Fails on `main` (the copy emitted no
    /// cast/copy event, so no trigger was placed).
    #[test]
    fn magecraft_trigger_fires_when_a_sorcery_is_copied() {
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::card_type::CoreType;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Magecraft permanent on the battlefield controlled by P0.
        let witch_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Sedgemoor Witch".to_string(),
            Zone::Battlefield,
        );
        {
            let witch = state.objects.get_mut(&witch_id).unwrap();
            witch.card_types.core_types.push(CoreType::Creature);
            witch.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::SpellCastOrCopy)
                    .valid_card(TargetFilter::Any)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )),
            );
        }

        // A sorcery spell on the stack, controlled by the Magecraft player.
        let sorcery = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            sorcery,
            CastingVariant::Normal,
        );

        // Drive the real copy resolver.
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // The copy emitted a `SpellCopied` event.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::SpellCopied { .. })),
            "copy resolver must emit SpellCopied"
        );
        // Two spells on the stack plus the Magecraft trigger parked by resolve.
        assert_eq!(state.stack.len(), 3);

        // CR 707.10: the Magecraft trigger landed on the stack.
        let magecraft_triggers = state
            .stack
            .iter()
            .filter(|e| {
                matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. }
                    if *source_id == witch_id)
            })
            .count();
        assert_eq!(
            magecraft_triggers, 1,
            "Magecraft (SpellCastOrCopy) must fire exactly once when a spell is copied"
        );
    }

    /// CR 707.10: "a copy of a spell isn't cast." A `SpellCast`-only trigger
    /// (Prowess-style) must NOT fire when a spell is merely copied. Guards the
    /// discriminator: emitting `SpellCast` for a copy would be rules-incorrect.
    #[test]
    fn spell_cast_only_trigger_does_not_fire_when_a_spell_is_copied() {
        use crate::game::triggers::process_triggers;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::card_type::CoreType;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // A SpellCast-only observer on the battlefield controlled by P0.
        let observer_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Cast Observer".to_string(),
            Zone::Battlefield,
        );
        {
            let observer = state.objects.get_mut(&observer_id).unwrap();
            observer.card_types.core_types.push(CoreType::Creature);
            observer.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::SpellCast)
                    .valid_card(TargetFilter::Any)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )),
            );
        }

        let sorcery = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            sorcery,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();
        process_triggers(&mut state, &events);

        // CR 707.10: no SpellCast-only trigger landed — a copy isn't cast.
        let cast_triggers = state
            .stack
            .iter()
            .filter(|e| {
                matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. }
                    if *source_id == observer_id)
            })
            .count();
        assert_eq!(
            cast_triggers, 0,
            "a SpellCast-only trigger must not fire on a copied (not cast) spell"
        );
    }

    #[test]
    fn copy_targeted_triggered_ability_on_stack_through_stack_resolution() {
        let mut state = GameState::new_two_player(42);
        let hope_id = ObjectId(10);
        let gogo_id = ObjectId(20);
        state.objects.insert(
            hope_id,
            GameObject::new(
                hope_id,
                CardId(10),
                PlayerId(0),
                "Hope Estheim".to_string(),
                Zone::Battlefield,
            ),
        );
        state.objects.insert(
            gogo_id,
            GameObject::new(
                gogo_id,
                CardId(20),
                PlayerId(0),
                "Gogo, Master of Mimicry".to_string(),
                Zone::Battlefield,
            ),
        );

        let hope_trigger_entry = ObjectId(30);
        let hope_trigger = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                ),
                destination: Zone::Graveyard,
            },
            vec![],
            hope_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: hope_trigger_entry,
            source_id: hope_id,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: hope_id,
                ability: Box::new(hope_trigger),
                condition: None,
                trigger_event: None,
                description: Some("At the beginning of your end step".to_string()),
                source_name: "Hope Estheim".to_string(),
                subject_match_count: None,
                die_result: None,
            },
        });
        state.stack.push_back(StackEntry {
            id: ObjectId(31),
            source_id: ObjectId(31),
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(31),
                ability: Box::new(ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    vec![],
                    ObjectId(31),
                    PlayerId(1),
                )),
                condition: None,
                trigger_event: None,
                description: Some("Opponent trigger".to_string()),
                source_name: "Opponent Source".to_string(),
                subject_match_count: None,
                die_result: None,
            },
        });

        let gogo_entry = ObjectId(40);
        let gogo_target_filter = TargetFilter::StackAbility {
            controller: Some(crate::types::ability::ControllerRef::You),
            tag: None,
            kind: None,
        };
        assert_eq!(
            crate::game::targeting::find_legal_targets(
                &state,
                &gogo_target_filter,
                PlayerId(0),
                gogo_id,
            ),
            vec![TargetRef::Object(hope_trigger_entry)]
        );

        let mut gogo_copy = ResolvedAbility::new(
            Effect::CopySpell {
                target: gogo_target_filter,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![TargetRef::Object(hope_trigger_entry)],
            gogo_id,
            PlayerId(0),
        );
        gogo_copy.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
        state.stack.push_back(StackEntry {
            id: gogo_entry,
            source_id: gogo_id,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: gogo_id,
                ability: gogo_copy,
            },
        });

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(state.stack.len(), 4);
        assert_eq!(state.stack[0].id, hope_trigger_entry);
        assert_eq!(state.stack[1].id, ObjectId(31));
        assert!(state.stack.iter().skip(2).all(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == hope_id
        )));
        assert!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
                .count()
                >= 2
        );
    }

    /// Put a Twinning Staff–style permanent (a `CopySpell` replacement with
    /// `Plus { value: 1 }`) onto the battlefield under `controller`.
    fn push_twinning_staff_in_zone(
        state: &mut GameState,
        obj_id: ObjectId,
        controller: PlayerId,
        zone: Zone,
    ) {
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut obj = GameObject::new(
            obj_id,
            CardId(900),
            controller,
            "Twinning Staff".to_string(),
            zone,
        );
        obj.controller = controller;
        obj.replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::CopySpell)
            .quantity_modification(QuantityModification::Plus { value: 1 })]
        .into();
        state.objects.insert(obj_id, obj);
        if zone == Zone::Battlefield {
            state.battlefield.push_back(obj_id);
        }
    }

    /// Put a Twinning Staff-style permanent on the battlefield under `controller`.
    fn push_twinning_staff(state: &mut GameState, obj_id: ObjectId, controller: PlayerId) {
        push_twinning_staff_in_zone(state, obj_id, controller, Zone::Battlefield);
    }

    /// Build a `CopySpell` ability (no targets → copies top of stack) for `controller`.
    fn copy_top_ability(controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(800),
            controller,
        )
    }

    /// CR 707.10 + CR 614.1a: Twinning Staff turns a single spell copy into two.
    #[test]
    fn copy_count_with_replacements_adds_one_for_twinning_staff() {
        let mut state = GameState::new_two_player(42);
        push_twinning_staff(&mut state, ObjectId(50), PlayerId(0));

        let spell = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            spell,
            CastingVariant::Normal,
        );

        let copy = copy_top_ability(PlayerId(0));
        assert_eq!(copy_count_with_replacements(&state, &copy, 1), 2);
    }

    /// CR 614.1: "If you would copy a spell *one or more times*" — a replacement
    /// effect watches for an event that would happen; when the base copy count is
    /// zero (e.g. a "copy for each X" with X = 0) there is no copy event, so
    /// Twinning Staff must NOT manufacture one.
    #[test]
    fn copy_count_with_replacements_does_not_apply_to_zero_copies() {
        let mut state = GameState::new_two_player(42);
        push_twinning_staff(&mut state, ObjectId(50), PlayerId(0));

        let spell = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            spell,
            CastingVariant::Normal,
        );

        let copy = copy_top_ability(PlayerId(0));
        assert_eq!(copy_count_with_replacements(&state, &copy, 0), 0);
    }

    /// CR 707.10: "If YOU would copy" — only the copying player's Twinning Staff
    /// applies. An opponent's Staff must not modify the count.
    #[test]
    fn copy_count_with_replacements_ignores_opponents_staff() {
        let mut state = GameState::new_two_player(42);
        push_twinning_staff(&mut state, ObjectId(50), PlayerId(1));

        let spell = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            spell,
            CastingVariant::Normal,
        );

        let copy = copy_top_ability(PlayerId(0));
        assert_eq!(copy_count_with_replacements(&state, &copy, 1), 1);
    }

    /// CR 113.6: Twinning Staff's static replacement functions only while the
    /// permanent is on the battlefield. The copy-count hook must not treat a card
    /// in a hidden or non-battlefield zone as an active replacement source.
    #[test]
    fn copy_count_with_replacements_ignores_staff_in_hand() {
        let mut state = GameState::new_two_player(42);
        push_twinning_staff_in_zone(&mut state, ObjectId(50), PlayerId(0), Zone::Hand);

        let spell = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            spell,
            CastingVariant::Normal,
        );

        let copy = copy_top_ability(PlayerId(0));
        assert_eq!(copy_count_with_replacements(&state, &copy, 1), 1);
    }

    /// CR 707.10: Copying an *ability* (not a spell) is unaffected by Twinning
    /// Staff. With only a triggered ability on the stack, the count is unchanged.
    #[test]
    fn copy_count_with_replacements_excludes_ability_copies() {
        let mut state = GameState::new_two_player(42);
        push_twinning_staff(&mut state, ObjectId(50), PlayerId(0));

        let trigger = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(11),
            PlayerId(0),
        );
        push_trigger(&mut state, ObjectId(11), CardId(2), PlayerId(0), trigger);

        let copy = copy_top_ability(PlayerId(0));
        assert_eq!(copy_count_with_replacements(&state, &copy, 1), 1);
    }

    /// CR 707.10 + CR 614.5: Regression — copying a *targeted* spell with
    /// Twinning Staff must make exactly TWO copies, not a runaway. A replacement
    /// effect gets only one opportunity to affect an event (CR 614.5). Each copy
    /// pauses on `CopyRetarget` and the drain driver resumes the next iteration;
    /// without the `copy_count_status` guard, every resumed iteration
    /// re-applied the +1 bonus and the loop exploded into dozens of copies (the
    /// in-game "stuck in a loop" report).
    #[test]
    fn twinning_staff_targeted_copy_does_not_runaway() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        push_twinning_staff(&mut state, ObjectId(50), PlayerId(0));

        // A creature for the copied spell to target.
        let mut bear = GameObject::new(
            ObjectId(60),
            CardId(5),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        bear.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(ObjectId(60), bear);

        // A targeted instant on the stack (Lightning Bolt-style), controlled by P0.
        let spell = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(ObjectId(60))],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            spell,
            CastingVariant::Normal,
        );

        // Resolve a "copy target spell, you may choose new targets" effect.
        let copy = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(70),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let _ = crate::game::effects::resolve_ability_chain(&mut state, &copy, &mut events, 0);

        // Drive each per-copy retarget pause to completion (keep current targets).
        let mut guard = 0;
        while let WaitingFor::CopyRetarget { player, .. } = state.waiting_for.clone() {
            guard += 1;
            assert!(
                guard < 12,
                "runaway copy loop: the copy_count_status guard failed to stop re-expansion"
            );
            state.waiting_for = WaitingFor::Priority { player };
            state.priority_player = player;
            crate::game::effects::drain_pending_continuation(&mut state, &mut events);
        }

        // Exactly two spell copies (base 1 + Twinning Staff's additional 1).
        let copies = state
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Stack)
            .count();
        assert_eq!(
            copies, 2,
            "Twinning Staff must make exactly one extra copy (2 total), got {copies}"
        );
    }

    /// Twinning Staff's ruling grants new-target permission for the replacement-
    /// added copy even if the original copy effect keeps targets unchanged.
    #[test]
    fn twinning_staff_added_copy_can_retarget_when_base_copy_cannot() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        push_twinning_staff(&mut state, ObjectId(50), PlayerId(0));

        let mut bear = GameObject::new(
            ObjectId(60),
            CardId(5),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        bear.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(ObjectId(60), bear);

        let spell = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(ObjectId(60))],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            spell,
            CastingVariant::Normal,
        );

        let copy = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            ObjectId(70),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let _ = crate::game::effects::resolve_ability_chain(&mut state, &copy, &mut events, 0);

        assert!(
            matches!(state.waiting_for, WaitingFor::CopyRetarget { .. }),
            "replacement-added copy must allow new targets"
        );
        let copies = state
            .objects
            .values()
            .filter(|o| o.is_token && o.zone == Zone::Stack)
            .count();
        assert_eq!(copies, 2);
    }

    /// CR 707.10 + CR 702.153a (issue #1159): Isochron Scepter copies an
    /// imprinted instant from exile via `TrackedSet`, not the top of stack.
    #[test]
    fn copy_spell_tracked_set_copies_exiled_imprint_not_top_of_stack() {
        let mut state = GameState::new_two_player(42);
        let scepter_id = ObjectId(5);
        let imprint_id = ObjectId(10);
        let decoy_spell_id = ObjectId(20);

        let imprint_spell = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        let mut imprint_obj = GameObject::new(
            imprint_id,
            CardId(1),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Exile,
        );
        imprint_obj.abilities = std::sync::Arc::new(vec![imprint_spell]);
        state.objects.insert(imprint_id, imprint_obj);

        state
            .tracked_object_sets
            .insert(TrackedSetId(0), vec![imprint_id]);

        let decoy_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            decoy_spell_id,
            PlayerId(0),
        );
        push_spell(
            &mut state,
            decoy_spell_id,
            CardId(2),
            PlayerId(0),
            "Divination",
            decoy_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: vec![],
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            scepter_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();

        assert_eq!(state.stack.len(), 2, "decoy plus imprint copy");
        let copy_entry = state.stack.back().unwrap();
        assert!(
            copy_entry
                .ability()
                .is_some_and(|a| matches!(a.effect, Effect::DealDamage { .. })),
            "copy must replicate the exiled imprint, not the decoy draw spell"
        );
    }

    /// CR 707.10 + CR 702.153a: tracked-set copying is an exiled-card source
    /// selector, so an invalid tracked object must not fall through to the top
    /// stack entry and copy an unrelated spell.
    #[test]
    fn copy_spell_tracked_set_without_spell_ability_does_not_fallback_to_stack_top() {
        let mut state = GameState::new_two_player(42);
        let scepter_id = ObjectId(5);
        let imprint_id = ObjectId(10);
        let decoy_spell_id = ObjectId(20);

        let non_spell_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mut imprint_obj = GameObject::new(
            imprint_id,
            CardId(1),
            PlayerId(0),
            "Activated Imprint".to_string(),
            Zone::Exile,
        );
        imprint_obj.abilities = std::sync::Arc::new(vec![non_spell_ability]);
        state.objects.insert(imprint_id, imprint_obj);
        state
            .tracked_object_sets
            .insert(TrackedSetId(0), vec![imprint_id]);

        let decoy_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            decoy_spell_id,
            PlayerId(0),
        );
        push_spell(
            &mut state,
            decoy_spell_id,
            CardId(2),
            PlayerId(0),
            "Divination",
            decoy_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: vec![],
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![],
            scepter_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        assert!(
            resolve(&mut state, &copy_ability, &mut events).is_err(),
            "invalid tracked imprint must not copy the unrelated top stack spell"
        );
        assert_eq!(state.stack.len(), 1, "only the decoy spell remains");
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::StackPushed { .. })));
    }

    /// CR 702.153a + CR 306.5b: Ob Nixilis Casualty copies stamp starting
    /// loyalty on the entering face (not live counters) so ETB seeding survives
    /// the stack → battlefield zone change.
    #[test]
    fn casualty_planeswalker_copy_enters_with_sacrifice_power_loyalty() {
        use crate::game::game_object::GameObject;
        use crate::game::stack::resolve_top;
        use crate::types::ability::CostPaidObjectSnapshot;
        use crate::types::card_type::{CardType, CoreType, Supertype};
        use crate::types::counter::CounterType;
        use crate::types::game_state::LKISnapshot;

        let mut state = GameState::new_two_player(42);
        let mut original = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![],
                duration: None,
                target: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        original.cost_paid_object = Some(CostPaidObjectSnapshot {
            object_id: ObjectId(99),
            lki: LKISnapshot {
                name: "Sacrifice".to_string(),
                power: Some(4),
                toughness: Some(4),
                base_power: Some(4),
                base_toughness: Some(4),
                mana_value: 4,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: vec![],
                counters: Default::default(),
                tapped: false,
                is_suspected: false,
            },
        });

        let mut obj = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Ob Nixilis, the Adversary".to_string(),
            Zone::Stack,
        );
        obj.card_types = CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Planeswalker],
            subtypes: vec!["Nixilis".to_string()],
        };
        obj.base_loyalty = Some(3);
        obj.loyalty = Some(3);
        state.objects.insert(ObjectId(10), obj);
        state.stack.push_back(StackEntry {
            id: ObjectId(10),
            source_id: ObjectId(10),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(original),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
                starting_loyalty_from_casualty_sacrifice: true,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &copy_ability, &mut events).unwrap();

        let copy_id = state.stack.back().unwrap().id;
        resolve_top(&mut state, &mut events);

        let copy = state.objects.get(&copy_id).expect("copy permanent");
        assert_eq!(copy.zone, Zone::Battlefield);
        assert!(
            !copy.card_types.supertypes.contains(&Supertype::Legendary),
            "Casualty copy must not be legendary"
        );
        assert_eq!(copy.loyalty, Some(4));
        assert_eq!(
            copy.counters.get(&CounterType::Loyalty).copied(),
            Some(4),
            "ETB must seed loyalty counters from the stamped entering face"
        );
    }
}

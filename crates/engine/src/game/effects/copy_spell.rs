use crate::types::ability::{
    CopyRetargetPermission, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CopyTargetSlot, GameState, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
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
    // copy this spell"): the targeted player.
    let copy_controller = copy_controller(ability);

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
        state.objects.insert(copy_id, copy_obj);
    }

    // CR 707.10: The copy has the same characteristics as the original, but its
    // identity is distinct.
    //   - Reset additional_cost_paid + kickers_paid so any "if its [additional]
    //     cost was paid" triggers (Offspring ETB, Casualty) do not fire for the
    //     copy — the copy is placed on the stack, not cast.
    //   - Update internal source_id references throughout the ability chain to
    //     copy_id. A copy is a new object — every `SelfRef` in its chain
    //     (including a nested `CopySpell` sub-ability, as in the Chain cycle)
    //     must resolve to the copy, not the original spell, or a
    //     second-generation copy would fail to find its source.
    //   - Re-controller the resolved ability chain so opponent-controlled copies
    //     (Twincast, Gogo) resolve under the copying player.
    let copy_kind = {
        let mut kind = top_entry.kind.clone();
        if let StackEntryKind::Spell {
            ability: Some(ref mut a),
            ..
        } = kind
        {
            set_resolved_source_recursive(a, copy_id);
            a.context.additional_cost_paid = false;
            a.context.kickers_paid.clear();
        }
        set_copied_kind_controller(&mut kind, copy_controller);
        kind
    };

    // CR 707.10: The copy's source_id is its own id (not the original's).
    let copy_entry = StackEntry {
        id: copy_id,
        source_id: copy_id,
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
        events.push(GameEvent::SpellCopied {
            card_id,
            controller: copy_controller,
            object_id: copy_id,
            original_id: top_entry.id,
        });
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
        // Compute legal alternatives for each slot so the UI can present valid
        // choices. If build_target_slots fails (no legal targets exist for the
        // copy), fall back to empty alternatives — the copy still goes on the
        // stack and will fizzle at resolution per CR 608.2b if all targets remain
        // illegal.
        // Use the copy's ability (with copy_id as source_id) so protection and
        // hexproof checks reflect the copy's identity, not the original's.
        let selection_slots = top_entry
            .ability()
            .map(|a| {
                let mut copy_ability = a.clone();
                copy_ability.source_id = copy_id;
                copy_ability
            })
            .and_then(|a| super::super::ability_utils::build_target_slots(state, &a).ok())
            .unwrap_or_default();

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

        // CR 707.10c: "its controller may choose new targets for the copy" —
        // the copy's controller makes the retarget choice.
        state.waiting_for = WaitingFor::CopyRetarget {
            player: copy_controller,
            copy_id,
            target_slots,
            current_slot: 0,
        };
        // EffectResolved deferred until after retarget choice completes.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
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

fn copy_source_entry(state: &GameState, ability: &ResolvedAbility) -> Option<StackEntry> {
    let target_id = ability.targets.iter().find_map(|target| match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    });
    if let Some(target_id) = target_id {
        return state
            .stack
            .iter()
            .find(|entry| entry.id == target_id)
            .cloned();
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
    state.stack.last().cloned()
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
fn set_resolved_source_recursive(ability: &mut ResolvedAbility, source_id: ObjectId) {
    ability.source_id = source_id;
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        set_resolved_source_recursive(sub_ability, source_id);
    }
    if let Some(else_ability) = ability.else_ability.as_mut() {
        set_resolved_source_recursive(else_ability, source_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{
        CopyRetargetPermission, Effect, QuantityExpr, TargetFilter, TargetRef,
    };
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
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

    #[test]
    fn test_copy_spell_empty_stack_returns_error() {
        let mut state = GameState::new_two_player(42);
        assert!(state.stack.is_empty());

        let ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
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
                trigger_event: None,
                description: None,
                source_name: String::new(),
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
                random: false,
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
    fn uncopyable_activated_ability_on_stack_is_not_copied_through_stack_resolution() {
        let mut state = GameState::new_two_player(42);
        let gogo_id = ObjectId(20);
        let other_id = ObjectId(21);

        let mut gogo_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::StackAbility {
                    controller: Some(crate::types::ability::ControllerRef::You),
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
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
                },
                retarget: CopyRetargetPermission::KeepOriginalTargets,
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
        use crate::game::triggers::process_triggers;
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
        // Two spells on the stack: original + copy.
        assert_eq!(state.stack.len(), 2);

        // Drive the normal post-event trigger pass with the resolver's events.
        process_triggers(&mut state, &events);

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
            },
        });

        let gogo_entry = ObjectId(40);
        let gogo_target_filter = TargetFilter::StackAbility {
            controller: Some(crate::types::ability::ControllerRef::You),
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
}

use crate::game::effects::destroy::{self, DestroyOutcome};
use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::game::targeting;
use crate::game::zones;
use crate::types::ability::{
    CounterSourceRider, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastingVariant, GameState, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// Counter target spells or abilities on the stack.
/// Spells are removed from the stack and moved to graveyard.
/// Abilities are simply removed from the stack (they aren't cards).
/// Respects CantBeCountered static ability.
///
/// CR 118.12: "Counter target spell unless its controller pays {X}" is no
/// longer handled here. The unless-pay modifier travels on
/// `ResolvedAbility.unless_pay` and is intercepted by the unified pipeline
/// in `game::effects::mod` BEFORE this resolver runs. By the time we reach
/// `resolve`, either the player declined to pay (so the counter goes
/// through unconditionally) or there was no unless-pay to begin with.
///
/// CR 701.6 + CR 608.2c: If the effect carries a `source_rider`, it runs as a
/// follow-up instruction acting on the countered ability's source permanent —
/// only when an *ability* (not a spell) was countered (CR 110.1 / CR 701.8a: a
/// spell is not a permanent). `CounterSourceRider::LosesAbilities` registers a
/// continuous "loses all abilities for as long as ~" static (Tidebinder);
/// `CounterSourceRider::Destroy` destroys the permanent (Teferi's Response,
/// Green Slime).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_rider = match &ability.effect {
        Effect::Counter { source_rider, .. } => source_rider.clone(),
        _ => None,
    };

    let targets = match &ability.effect {
        Effect::Counter { target, .. } if matches!(target, TargetFilter::ParentTarget) => {
            let event_target = targeting::resolve_event_context_target(
                state,
                &TargetFilter::TriggeringSource,
                ability.source_id,
            );
            match event_target {
                Some(target) => vec![target],
                None => targeting::resolved_targets(ability, target, state),
            }
        }
        Effect::Counter { target, .. } => targeting::resolved_targets(ability, target, state),
        _ => ability.targets.clone(),
    };

    // CR 115.1: `Effect::Counter` is single-target by construction — mass
    // counter is `Effect::CounterAll`. The post-loop rider therefore acts on at
    // most one countered ability's source permanent.
    debug_assert!(
        targets.len() <= 1,
        "Effect::Counter must be single-target (mass counter is Effect::CounterAll)"
    );

    // CR 701.6 + CR 608.2c: When an *ability* is countered (not a spell), carry
    // its source permanent and the rider out of the loop so the follow-up
    // instruction runs after EffectResolved (see CR 110.1 / CR 701.8a: a
    // countered spell is not a permanent, so no rider fires for spells).
    let mut countered_ability_source: Option<ObjectId> = None;

    for target in targets {
        if let TargetRef::Object(obj_id) = target {
            // CR 101.2: Check if the target can't be countered.
            // Two paths: (1) battlefield permanents granting uncounterability
            // (e.g. "Spells you control can't be countered"), and (2) the
            // spell's own intrinsic static definition (e.g. Carnage Tyrant).
            let ctx = StaticCheckContext {
                source_id: Some(obj_id),
                target_id: Some(obj_id),
                ..Default::default()
            };
            if check_static_ability(state, StaticMode::CantBeCountered, &ctx) {
                continue;
            }

            // CR 702.26b + CR 114.4 + CR 604.1: route through the single-authority
            // helper so stack-resident spells (and any edge case that later
            // lands these definitions in a gated zone) get the same gating as
            // every other read site. Spells on the stack are not phased out
            // and not in the command zone, so the gate is a no-op for the
            // common path — this is about architectural consistency, not
            // behavior change.
            let has_cant_be_countered = state
                .objects
                .get(&obj_id)
                .map(|obj| {
                    super::super::functioning_abilities::active_static_definitions(state, obj)
                        .any(|sd| sd.mode == StaticMode::CantBeCountered)
                })
                .unwrap_or(false);
            if has_cant_be_countered {
                continue;
            }

            // Remove from stack — search by both id (spells) and source_id (abilities).
            // Use rposition to match the most recently pushed entry.
            let stack_idx = state
                .stack
                .iter()
                .rposition(|e| e.id == obj_id || e.source_id == obj_id);
            if let Some(idx) = stack_idx {
                let is_spell = matches!(state.stack[idx].kind, StackEntryKind::Spell { .. });
                // CR 702.34a / CR 702.127a / CR 702.180a: Flashback,
                // Aftermath, and Harmonize exile when leaving the stack for
                // any reason, including when countered. Escape (CR 702.138)
                // has no such clause — countered escape spells go to graveyard.
                let casting_variant = match &state.stack[idx].kind {
                    StackEntryKind::Spell {
                        casting_variant, ..
                    } => *casting_variant,
                    _ => CastingVariant::Normal,
                };
                let exiles_on_counter = casting_variant.replaces_stack_to_graveyard_with_exile();
                let source_permanent_id = state.stack[idx].source_id;
                let removed_entry_id = state.stack[idx].id;
                state.stack.remove(idx);
                state.stack_paid_facts.remove(&removed_entry_id);

                if is_spell {
                    // CR 608.2b: Countered spells go to graveyard, unless cast via an
                    // alt-cost keyword that exiles on leaving the stack (Flashback, Harmonize).
                    let dest = if exiles_on_counter {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    };
                    zones::move_to_zone(state, obj_id, dest, events);
                    if casting_variant.restores_front_face_after_stack_exit() {
                        super::super::stack::restore_alternative_spell_normal_face(state, obj_id);
                    }
                } else {
                    // CR 110.1 / CR 701.8a: An ability was countered, so its
                    // source is a permanent the rider can act on. Defer the
                    // rider to the post-loop block so EffectResolved precedes
                    // any WaitingFor a replacement choice may set.
                    countered_ability_source = Some(source_permanent_id);
                }

                events.push(GameEvent::SpellCountered {
                    object_id: obj_id,
                    countered_by: ability.source_id,
                    countered_by_controller: ability.controller,
                });
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    // CR 608.2c: The rider is a follow-up instruction conditional on the prior
    // counter outcome — it runs only when an ability (not a spell) was actually
    // countered. EffectResolved is already pushed above, so an early return on a
    // mid-resolution replacement choice loses nothing.
    if let Some(source_permanent_id) = countered_ability_source {
        match source_rider {
            // CR 611.2: Apply the "loses all abilities ..." static to the
            // countered ability's source permanent (Tishana's Tidebinder).
            Some(CounterSourceRider::LosesAbilities { static_def }) => {
                apply_source_static(state, ability.source_id, source_permanent_id, &static_def);
            }
            // CR 701.8: Destroy the countered ability's source permanent
            // (Teferi's Response, Green Slime) through the shared guarded path
            // so emblem (CR 114.5), zone, and indestructible (CR 702.12b)
            // guards cannot be bypassed.
            Some(CounterSourceRider::Destroy) => {
                match destroy::destroy_single_object(
                    state,
                    source_permanent_id,
                    ability.source_id,
                    // CR 701.8: "destroy that permanent" with no "can't be
                    // regenerated" clause.
                    false,
                    events,
                ) {
                    DestroyOutcome::Completed | DestroyOutcome::Skipped => {}
                    // `state.waiting_for` is set by the replacement pipeline.
                    DestroyOutcome::NeedsChoice => return Ok(()),
                }
            }
            None => {}
        }
    }

    Ok(())
}

/// CR 701.6 + CR 405.1: Mass counter — iterate every stack entry and counter
/// each one that matches the class filter. Mirrors `destroy::resolve_all` in
/// shape: collect matching IDs, then run the same removal/zone-move logic the
/// single-target `resolve` uses (re-using `CR 702.34a` Flashback exile-on-
/// counter and `CR 608.2b` countered-spell-to-graveyard rules).
///
/// Stack entry matching is delegated to `targeting::stack_entry_matches_filter`
/// so `CounterAll` shares the same `StackSpell`, `StackAbility`, typed,
/// controller, and stack-target-constraint semantics as ordinary stack
/// targeting.
///
/// CR 101.2 / CR 614.5: `CantBeCountered` is honored per-entry in the same
/// loop the single-target counter uses. CR 118.12 ("unless pays") does not
/// apply: mass counter is non-targeting (CR 115.1), so no controller is given
/// the opt-out choice — and no current corpus card combines mass counter with
/// an unless-cost.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::CounterAll { target } => target.clone(),
        _ => TargetFilter::None,
    };

    // CR 405.2: Iterate the stack from the bottom upward, collecting every
    // entry that matches. Snapshot the object IDs first so we can mutate
    // `state.stack` afterward without invalidating an active borrow.
    let matching: Vec<ObjectId> = state
        .stack
        .iter()
        .filter(|entry| {
            targeting::stack_entry_matches_filter(
                state,
                entry,
                &target_filter,
                ability.controller,
                ability.source_id,
            )
        })
        .map(|entry| entry.id)
        .collect();

    for obj_id in matching {
        // CR 101.2: Per-entry CantBeCountered guard — same logic the
        // single-target resolver uses.
        let s_ctx = StaticCheckContext {
            source_id: Some(obj_id),
            target_id: Some(obj_id),
            ..Default::default()
        };
        if check_static_ability(state, StaticMode::CantBeCountered, &s_ctx) {
            continue;
        }
        let has_cant_be_countered = state
            .objects
            .get(&obj_id)
            .map(|obj| {
                super::super::functioning_abilities::active_static_definitions(state, obj)
                    .any(|sd| sd.mode == StaticMode::CantBeCountered)
            })
            .unwrap_or(false);
        if has_cant_be_countered {
            continue;
        }

        // CR 405.2: Look up the stack entry by its own id only. The
        // `matching` set was populated from `entry.id`, so a `source_id`
        // fallback (used in the single-target resolver to bridge a target's
        // ObjectId to its parent permanent) would match the wrong entry
        // when several stack entries share a `source_id` (e.g., two
        // activated abilities of the same permanent).
        let stack_idx = state.stack.iter().position(|e| e.id == obj_id);
        let Some(idx) = stack_idx else { continue };

        let is_spell = matches!(state.stack[idx].kind, StackEntryKind::Spell { .. });
        // CR 702.34a / CR 702.127a / CR 702.180a: Flashback / Aftermath /
        // Harmonize exile on leaving the stack for any reason, including
        // counter. Escape (CR 702.138) has no such clause.
        let casting_variant = match &state.stack[idx].kind {
            StackEntryKind::Spell {
                casting_variant, ..
            } => *casting_variant,
            _ => CastingVariant::Normal,
        };
        let exiles_on_counter = casting_variant.replaces_stack_to_graveyard_with_exile();
        let removed_entry_id = state.stack[idx].id;
        state.stack.remove(idx);
        state.stack_paid_facts.remove(&removed_entry_id);

        if is_spell {
            // CR 608.2b: Countered spells go to graveyard, unless cast via an
            // alt-cost keyword that exiles on leaving the stack.
            let dest = if exiles_on_counter {
                Zone::Exile
            } else {
                Zone::Graveyard
            };
            zones::move_to_zone(state, obj_id, dest, events);
            if casting_variant.restores_front_face_after_stack_exit() {
                super::super::stack::restore_alternative_spell_normal_face(state, obj_id);
            }
        }
        // For abilities, removing the stack entry above is sufficient — they
        // aren't cards and have no zone to move to.

        events.push(GameEvent::SpellCountered {
            object_id: obj_id,
            countered_by: ability.source_id,
            countered_by_controller: ability.controller,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 611.2: Register a transient continuous effect for a counter's
/// `CounterSourceRider::LosesAbilities` static.
///
/// The effect targets the countered ability's source permanent and persists
/// as long as the counter source (e.g., Tidebinder) remains on the battlefield.
fn apply_source_static(
    state: &mut GameState,
    counter_source_id: ObjectId,
    source_permanent_id: ObjectId,
    static_def: &StaticDefinition,
) {
    // Only apply if the source permanent is still on the battlefield
    if !state.battlefield.contains(&source_permanent_id) {
        return;
    }

    let controller = state
        .objects
        .get(&counter_source_id)
        .map(|o| o.controller)
        .unwrap_or_default();

    state.add_transient_continuous_effect(
        counter_source_id,
        controller,
        Duration::UntilHostLeavesPlay,
        TargetFilter::SpecificObject {
            id: source_permanent_id,
        },
        static_def.modifications.clone(),
        static_def.condition.clone(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, Duration, Effect, FilterProp, KeywordAction,
        StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;

    #[test]
    fn counter_removes_from_stack_and_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.stack.is_empty());
        assert!(state.players[1].graveyard.contains(&obj_id));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::SpellCountered { .. })));
    }

    #[test]
    fn graveyard_permission_exile_rider_exiles_countered_spell() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::GraveyardPermission {
                    source: ObjectId(200),
                    frequency: crate::types::statics::CastFrequency::OncePerTurn,
                    slot_type: None,
                    graveyard_destination_replacement: Some(Zone::Exile),
                },
                actual_mana_spent: 0,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.stack.is_empty());
        assert!(state.exile.contains(&obj_id));
        assert!(!state.players[1].graveyard.contains(&obj_id));
    }

    #[test]
    fn cant_be_countered_spell_stays_on_stack() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Uncounterable".to_string(),
            Zone::Stack,
        );
        // Add CantBeCountered static definition to the spell
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeCountered));
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Spell should still be on the stack (not countered)
        assert_eq!(state.stack.len(), 1);
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::SpellCountered { .. })));
    }

    #[test]
    fn counter_ability_applies_source_static_to_counter_source() {
        let mut state = GameState::new_two_player(42);

        // Source permanent on the battlefield (e.g., a creature whose ability was activated)
        let source_permanent = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Source Creature".to_string(),
            Zone::Battlefield,
        );

        // Tidebinder on the battlefield (the counter source)
        let tidebinder = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Tidebinder".to_string(),
            Zone::Battlefield,
        );

        // Triggered ability on the stack (from the source creature)
        let ability_on_stack = ObjectId(999);
        state.stack.push_back(StackEntry {
            id: ability_on_stack,
            source_id: source_permanent,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source_permanent,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    source_permanent,
                    PlayerId(1),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });

        let source_static = StaticDefinition::continuous()
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::StackAbility { controller: None },
                source_rider: Some(CounterSourceRider::LosesAbilities {
                    static_def: Box::new(source_static),
                }),
            },
            vec![TargetRef::Object(ability_on_stack)],
            tidebinder,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // Ability should be removed from stack
        assert!(state.stack.is_empty(), "ability should be countered");

        // Should register a transient continuous effect targeting the source permanent
        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "Should have one transient continuous effect"
        );
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.source_id, tidebinder, "source should be Tidebinder");
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificObject {
                id: source_permanent
            },
            "should target the source permanent"
        );
        assert_eq!(
            tce.duration,
            Duration::UntilHostLeavesPlay,
            "should persist while Tidebinder is on battlefield"
        );
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::RemoveAllAbilities],
            "should remove all abilities"
        );
    }

    #[test]
    fn counter_spell_does_not_apply_source_static() {
        let mut state = GameState::new_two_player(42);

        let tidebinder = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Tidebinder".to_string(),
            Zone::Battlefield,
        );

        // A spell on the stack (not an ability)
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let source_static = StaticDefinition::continuous()
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: Some(CounterSourceRider::LosesAbilities {
                    static_def: Box::new(source_static),
                }),
            },
            vec![TargetRef::Object(spell_id)],
            tidebinder,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // Spell countered, but the rider should NOT be applied (it's a spell,
        // not an ability — CR 110.1 / CR 701.8a).
        assert!(
            state.transient_continuous_effects.is_empty(),
            "source_rider should not apply when countering a spell"
        );
    }

    /// CR 701.8: Countering an *ability* with the Destroy rider destroys the
    /// ability's source permanent (Teferi's Response, Green Slime).
    #[test]
    fn counter_ability_destroy_rider_destroys_source_permanent() {
        let mut state = GameState::new_two_player(42);

        // The source permanent whose ability is on the stack.
        let source_permanent = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Source Creature".to_string(),
            Zone::Battlefield,
        );

        // The counter source (e.g. Green Slime).
        let counter_source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Green Slime".to_string(),
            Zone::Battlefield,
        );

        let ability_on_stack = ObjectId(999);
        state.stack.push_back(StackEntry {
            id: ability_on_stack,
            source_id: source_permanent,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source_permanent,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    source_permanent,
                    PlayerId(1),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::StackAbility { controller: None },
                source_rider: Some(CounterSourceRider::Destroy),
            },
            vec![TargetRef::Object(ability_on_stack)],
            counter_source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // CR 701.6a: ability removed from the stack.
        assert!(state.stack.is_empty(), "ability should be countered");
        // CR 701.8a: the source permanent moved battlefield → graveyard.
        assert!(
            !state.battlefield.contains(&source_permanent),
            "source permanent should leave the battlefield"
        );
        assert!(
            state.players[1].graveyard.contains(&source_permanent),
            "source permanent should be in its owner's graveyard"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::CreatureDestroyed { object_id } if *object_id == source_permanent)),
            "a destroy event should fire for the source permanent"
        );
    }

    /// CR 701.8a / CR 110.1 discriminator: countering a *spell* with the Destroy
    /// rider destroys nothing — a spell on the stack is not a permanent, so the
    /// rider does not fire. (This is the spell-vs-ability gate, the structural
    /// encoding of "if a permanent's ability is countered this way".)
    #[test]
    fn counter_spell_destroy_rider_destroys_nothing() {
        let mut state = GameState::new_two_player(42);

        let counter_source = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Teferi's Response".to_string(),
            Zone::Battlefield,
        );

        // A battlefield permanent recorded as the countered spell's stack
        // `source_id`. This is the sharp CR 110.1 discriminator: if the rider
        // fired on the spell-vs-ability gate's *wrong* (spell) side, this
        // battlefield permanent would be destroyed. The spell-vs-ability gate
        // must skip the rider entirely for spells, so this permanent survives —
        // independent of the destroy zone guard.
        let decoy_permanent = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Decoy Creature".to_string(),
            Zone::Battlefield,
        );

        // A spell on the stack (not an ability), whose `source_id` points at a
        // battlefield permanent.
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: decoy_permanent,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: Some(CounterSourceRider::Destroy),
            },
            vec![TargetRef::Object(spell_id)],
            counter_source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // CR 608.2b: the spell was countered into its owner's graveyard.
        assert!(state.stack.is_empty(), "spell should be countered");
        assert!(state.players[1].graveyard.contains(&spell_id));
        // CR 701.8a / CR 110.1: a countered spell is not a permanent — the
        // rider does not fire, so the battlefield permanent recorded as the
        // spell's source survives and no destroy event is produced.
        assert!(
            state.battlefield.contains(&decoy_permanent),
            "the destroy rider must not fire when a spell is countered (CR 110.1)"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::CreatureDestroyed { .. })),
            "no destroy event should fire when a spell is countered"
        );
    }

    #[test]
    fn flashback_spell_exiles_when_countered() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Flashback Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Flashback,
                actual_mana_spent: 0,
            },
        });

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // CR 702.34a: Flashback spell should exile when countered, not go to graveyard.
        assert_eq!(
            state.objects[&obj_id].zone,
            Zone::Exile,
            "Flashback spell should be exiled when countered"
        );
    }

    /// CR 118.12 (M1 fold): Post the 2026-05-09 fold, the counter resolver
    /// has no bespoke `unless_pay` branch — the modifier flows through the
    /// generic `ResolvedAbility.unless_pay` path in `effects::mod`. This
    /// test guards against re-introducing a counter-specific branch by
    /// verifying that the resolver itself unconditionally counters when
    /// invoked directly with no `unless_pay` (the `unless_pay` is consumed
    /// upstream before the ability reaches `counter::resolve`).
    #[test]
    fn counter_resolver_unconditionally_counters_when_unless_pay_consumed_upstream() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Build a Counter ability — no unless_pay set on the ResolvedAbility,
        // mirroring what reaches `counter::resolve` after the unified
        // `unless_pay` interceptor strips the modifier from `pending_effect`.
        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Spell counters unconditionally — the resolver does not search for
        // an unless modifier on `ability.unless_pay`, because the runtime
        // owns that gate at the call layer above.
        assert!(state.stack.is_empty(), "spell should be countered");
        assert!(state.players[1].graveyard.contains(&obj_id));
    }

    /// CR 701.6 + CR 405.1: Mass counter iterates the stack and counters every
    /// spell matching the class filter. Mixed-population test: P1 has two
    /// spells (matched by `StackSpell + Card + controller: Opponent`), P0 has one
    /// spell and one ability on the stack. Only P1's spells should leave the stack.
    #[test]
    fn test_counter_all_opponent_spells_filters_own_spells() {
        let mut state = GameState::new_two_player(42);
        // P1 (opponent of P0) has two spells on the stack.
        let p1_spell_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        let p1_spell_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Stack,
        );
        // P0 has one spell on the stack — should NOT be countered.
        let p0_spell = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Healing Salve".to_string(),
            Zone::Stack,
        );
        let p1_ability = ObjectId(901);
        // Stamp Instant card_type onto each so the filter evaluator
        // classifies them as Card/Spell objects.
        for id in [p1_spell_a, p1_spell_b, p0_spell] {
            let card_type = CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            };
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }
        for (id, controller) in [
            (p1_spell_a, PlayerId(1)),
            (p1_spell_b, PlayerId(1)),
            (p0_spell, PlayerId(0)),
        ] {
            state.stack.push_back(StackEntry {
                id,
                source_id: id,
                controller,
                kind: StackEntryKind::Spell {
                    card_id: CardId(0),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }
        state.stack.push_back(StackEntry {
            id: p1_ability,
            source_id: p1_spell_a,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: p1_spell_a,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    vec![],
                    p1_spell_a,
                    PlayerId(1),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });

        let opponent_spell_filter = TargetFilter::And {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: Some(ControllerRef::Opponent),
                    properties: vec![FilterProp::InZone { zone: Zone::Stack }],
                }),
            ],
        };

        // Glen Elendra-shape ability — controller is P0, so "your opponents"
        // resolves to P1.
        let ability = ResolvedAbility::new(
            Effect::CounterAll {
                target: opponent_spell_filter,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // P1's spells were countered → graveyard, removed from stack.
        assert_eq!(
            state.stack.len(),
            2,
            "P0's spell and P1's stack ability remain, P1's two spells countered"
        );
        assert!(state.stack.iter().any(|entry| entry.id == p0_spell));
        assert!(state.stack.iter().any(|entry| entry.id == p1_ability));
        assert!(state.players[1].graveyard.contains(&p1_spell_a));
        assert!(state.players[1].graveyard.contains(&p1_spell_b));
        assert!(!state.players[0].graveyard.contains(&p0_spell));
        // Two SpellCountered events emitted.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::SpellCountered { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn test_counter_all_artifact_spells_uses_composed_stack_spell_filter() {
        let mut state = GameState::new_two_player(42);
        let artifact_spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Arcane Signet".to_string(),
            Zone::Stack,
        );
        let instant_spell = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opt".to_string(),
            Zone::Stack,
        );

        {
            let card_type = CardType {
                core_types: vec![CoreType::Artifact],
                ..Default::default()
            };
            let obj = state.objects.get_mut(&artifact_spell).unwrap();
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }
        {
            let card_type = CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            };
            let obj = state.objects.get_mut(&instant_spell).unwrap();
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        for id in [artifact_spell, instant_spell] {
            state.stack.push_back(StackEntry {
                id,
                source_id: id,
                controller: PlayerId(1),
                kind: StackEntryKind::Spell {
                    card_id: CardId(0),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }

        let ability = ResolvedAbility::new(
            Effect::CounterAll {
                target: TargetFilter::And {
                    filters: vec![
                        TargetFilter::StackSpell,
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Artifact],
                            controller: None,
                            properties: vec![FilterProp::InZone { zone: Zone::Stack }],
                        }),
                    ],
                },
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.stack.iter().next().unwrap().id, instant_spell);
        assert!(state.players[1].graveyard.contains(&artifact_spell));
        assert!(!state.players[1].graveyard.contains(&instant_spell));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::SpellCountered { object_id, .. } if *object_id == artifact_spell))
                .count(),
            1
        );
    }

    /// CR 113.3 + CR 405.1: "Counter all abilities" — the resolver matches
    /// every activated/triggered ability on the stack, including keyword actions, via
    /// `TargetFilter::StackAbility { controller: None }` and removes the entry without moving any
    /// card to a graveyard (abilities aren't cards).
    #[test]
    fn test_counter_all_abilities_removes_ability_entries() {
        let mut state = GameState::new_two_player(42);
        let perm = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Source Permanent".to_string(),
            Zone::Battlefield,
        );

        // Two triggered abilities + one activated ability + one keyword action
        // + one spell on stack.
        let trig_a = ObjectId(901);
        let trig_b = ObjectId(902);
        let act = ObjectId(903);
        let keyword_action = ObjectId(904);
        let spell = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        for ab_id in [trig_a, trig_b] {
            state.stack.push_back(StackEntry {
                id: ab_id,
                source_id: perm,
                controller: PlayerId(1),
                kind: StackEntryKind::TriggeredAbility {
                    source_id: perm,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::Unimplemented {
                            name: "Trig".to_string(),
                            description: None,
                        },
                        vec![],
                        perm,
                        PlayerId(1),
                    )),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                },
            });
        }
        state.stack.push_back(StackEntry {
            id: act,
            source_id: perm,
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: perm,
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Act".to_string(),
                        description: None,
                    },
                    vec![],
                    perm,
                    PlayerId(1),
                ),
            },
        });
        state.stack.push_back(StackEntry {
            id: keyword_action,
            source_id: perm,
            controller: PlayerId(1),
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Crew {
                    vehicle_id: perm,
                    paid_creature_ids: vec![],
                },
            },
        });
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(20),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::CounterAll {
                target: TargetFilter::StackAbility { controller: None },
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All four ability/action entries removed; spell remains on stack.
        assert_eq!(state.stack.len(), 1, "only the spell remains");
        assert_eq!(state.stack.iter().next().unwrap().id, spell);
        // No card moved to graveyard (abilities aren't cards).
        assert!(state.players[1].graveyard.is_empty());
        // Four SpellCountered events for the ability/action entries.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::SpellCountered { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn test_counter_all_opponent_abilities_preserves_your_abilities() {
        let mut state = GameState::new_two_player(42);
        let your_perm = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Your Source".to_string(),
            Zone::Battlefield,
        );
        let opponent_perm = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opponent Source".to_string(),
            Zone::Battlefield,
        );
        let your_ability = ObjectId(910);
        let opponent_ability = ObjectId(911);
        for (entry_id, source_id, controller) in [
            (your_ability, your_perm, PlayerId(0)),
            (opponent_ability, opponent_perm, PlayerId(1)),
        ] {
            state.stack.push_back(StackEntry {
                id: entry_id,
                source_id,
                controller,
                kind: StackEntryKind::TriggeredAbility {
                    source_id,
                    ability: Box::new(ResolvedAbility::new(
                        Effect::Draw {
                            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                        vec![],
                        source_id,
                        controller,
                    )),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                },
            });
        }

        let ability = ResolvedAbility::new(
            Effect::CounterAll {
                target: TargetFilter::StackAbility {
                    controller: Some(ControllerRef::Opponent),
                },
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.stack.iter().next().unwrap().id, your_ability);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::SpellCountered { object_id, .. } if *object_id == opponent_ability))
                .count(),
            1
        );
    }
}

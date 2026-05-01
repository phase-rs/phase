use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::game::zones;
use crate::types::ability::{
    Duration, Effect, EffectError, EffectKind, ResolvedAbility, StaticDefinition, TargetFilter,
    TargetRef, UnlessCost,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastingVariant, GameState, StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// Counter target spells or abilities on the stack.
/// Spells are removed from the stack and moved to graveyard.
/// Abilities are simply removed from the stack (they aren't cards).
/// Respects CantBeCountered static ability.
///
/// If the effect carries `unless_payment`, the spell's controller is given the
/// choice to pay the cost. If they can and do pay, the spell is NOT countered.
/// CR 118.12.
///
/// If the effect carries a `source_static`, it is applied to the counter's source
/// (e.g., Tidebinder) with `affected: SpecificObject(source_permanent_id)` after
/// successfully countering a permanent's ability. This implements "that permanent
/// loses all abilities for as long as ~" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (source_static, unless_payment) = match &ability.effect {
        Effect::Counter {
            source_static,
            unless_payment,
            ..
        } => (source_static.clone(), unless_payment.clone()),
        _ => (None, None),
    };

    // CR 118.12: "Unless pays" — always present the choice to the spell's controller.
    // The player may activate mana abilities before deciding whether to pay.
    if let Some(ref unless_cost) = unless_payment {
        if let Some(TargetRef::Object(obj_id)) = ability.targets.first() {
            // Search by both id (spells) and source_id (abilities) — use rev() to
            // match the most recently pushed entry when a permanent has multiple
            // abilities on the stack.
            let target_controller = state
                .stack
                .iter()
                .rev()
                .find(|e| e.id == *obj_id || e.source_id == *obj_id)
                .map(|e| e.controller);

            if let Some(controller) = target_controller {
                let resolved_cost = resolve_unless_cost(unless_cost, state, ability);
                // CR 118.7: If the cost is {0}, the player is considered to have paid.
                if matches!(&resolved_cost, UnlessCost::Fixed { cost } if *cost == ManaCost::zero())
                {
                    // Effect is prevented — spell survives.
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::Counter,
                        source_id: ability.source_id,
                    });
                    return Ok(());
                }
                state.waiting_for = WaitingFor::UnlessPayment {
                    player: controller,
                    cost: resolved_cost,
                    pending_effect: Box::new(ability.clone()),
                    trigger_event: state.current_trigger_event.clone(),
                    effect_description: Some("counter target spell".to_string()),
                };
                return Ok(());
            }
        }
    }

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // CR 101.2: Check if the target can't be countered.
            // Two paths: (1) battlefield permanents granting uncounterability
            // (e.g. "Spells you control can't be countered"), and (2) the
            // spell's own intrinsic static definition (e.g. Carnage Tyrant).
            let ctx = StaticCheckContext {
                source_id: Some(*obj_id),
                target_id: Some(*obj_id),
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
                .get(obj_id)
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
                .rposition(|e| e.id == *obj_id || e.source_id == *obj_id);
            if let Some(idx) = stack_idx {
                let is_spell = matches!(state.stack[idx].kind, StackEntryKind::Spell { .. });
                // CR 702.34a / CR 702.127a / CR 702.180a: Flashback,
                // Aftermath, and Harmonize exile when leaving the stack for
                // any reason, including when countered. Escape (CR 702.138)
                // has no such clause — countered escape spells go to graveyard.
                let exiles_on_counter = matches!(
                    &state.stack[idx].kind,
                    StackEntryKind::Spell {
                        casting_variant: CastingVariant::Harmonize
                            | CastingVariant::Flashback
                            | CastingVariant::Aftermath,
                        ..
                    }
                );
                let source_permanent_id = state.stack[idx].source_id;
                state.stack.remove(idx);

                if is_spell {
                    // CR 608.2b: Countered spells go to graveyard, unless cast via an
                    // alt-cost keyword that exiles on leaving the stack (Flashback, Harmonize).
                    let dest = if exiles_on_counter {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    };
                    zones::move_to_zone(state, *obj_id, dest, events);
                } else {
                    // Ability was countered — apply source_static if present
                    apply_source_static(
                        state,
                        ability.source_id,
                        source_permanent_id,
                        &source_static,
                    );
                }

                events.push(GameEvent::SpellCountered {
                    object_id: *obj_id,
                    countered_by: ability.source_id,
                });
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.6 + CR 405.1: Mass counter — iterate every stack entry and counter
/// each one that matches the class filter. Mirrors `destroy::resolve_all` in
/// shape: collect matching IDs, then run the same removal/zone-move logic the
/// single-target `resolve` uses (re-using `CR 702.34a` Flashback exile-on-
/// counter and `CR 608.2b` countered-spell-to-graveyard rules).
///
/// Filter shapes recognized:
///   - `TargetFilter::Typed { properties: [InZone { Stack }, ...], ... }` —
///     spell entries are matched by `matches_target_filter` against the
///     spell's `state.objects[id]` (which lives in the Stack zone).
///   - `TargetFilter::StackAbility` — every activated/triggered ability on
///     the stack matches. Controller scoping is not currently supported by
///     this filter variant; for "counter all abilities your opponents
///     control" the filter widens to all abilities. Tracked in queue for a
///     follow-up that adds a controller slot to `StackAbility`.
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
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<ObjectId> = state
        .stack
        .iter()
        .filter(|entry| match &target_filter {
            // CR 113.3: Activated and triggered abilities on the stack.
            TargetFilter::StackAbility => matches!(
                entry.kind,
                StackEntryKind::ActivatedAbility { .. } | StackEntryKind::TriggeredAbility { .. }
            ),
            // CR 405.1 / CR 112.1: Spell entries are objects on the stack;
            // the typed filter sees them via `state.objects[entry.id]` and
            // the `InZone { Stack }` property.
            other => crate::game::filter::matches_target_filter(state, entry.id, other, &ctx),
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
        let exiles_on_counter = matches!(
            &state.stack[idx].kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Harmonize
                    | CastingVariant::Flashback
                    | CastingVariant::Aftermath,
                ..
            }
        );
        state.stack.remove(idx);

        if is_spell {
            // CR 608.2b: Countered spells go to graveyard, unless cast via an
            // alt-cost keyword that exiles on leaving the stack.
            let dest = if exiles_on_counter {
                Zone::Exile
            } else {
                Zone::Graveyard
            };
            zones::move_to_zone(state, obj_id, dest, events);
        }
        // For abilities, removing the stack entry above is sufficient — they
        // aren't cards and have no zone to move to.

        events.push(GameEvent::SpellCountered {
            object_id: obj_id,
            countered_by: ability.source_id,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Execute the counter unconditionally (used after opponent declines to pay
/// an "unless pays" cost, or when they can't pay at all).
/// Strips `unless_payment` to prevent re-entering the payment choice.
pub fn resolve_unconditional(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut ability = ability.clone();
    // Strip unless_payment to prevent re-prompting
    if let Effect::Counter {
        ref mut unless_payment,
        ..
    } = ability.effect
    {
        *unless_payment = None;
    }
    resolve(state, &ability, events)
}

/// Register a transient continuous effect for a counter's source_static.
///
/// The effect targets the countered ability's source permanent and persists
/// as long as the counter source (e.g., Tidebinder) remains on the battlefield.
fn apply_source_static(
    state: &mut GameState,
    counter_source_id: ObjectId,
    source_permanent_id: ObjectId,
    source_static: &Option<StaticDefinition>,
) {
    let static_def = match source_static {
        Some(def) => def,
        None => return,
    };

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

/// CR 118.12: Resolve an `UnlessCost` to a concrete cost.
/// For `Fixed`, returns as-is. For `DynamicGeneric`, evaluates the quantity
/// expression against current game state and returns `Fixed`.
/// Non-mana costs (`PayLife`, `DiscardCard`, `Sacrifice`) pass through unchanged.
pub(crate) fn resolve_unless_cost(
    cost: &UnlessCost,
    state: &GameState,
    ability: &ResolvedAbility,
) -> UnlessCost {
    match cost {
        UnlessCost::DynamicGeneric { quantity } => {
            // CR 107.1b: Ability context lets X-based unless-costs
            // ("pay X life" / "pay {X}") read the caster-chosen X.
            let amount = resolve_quantity_with_targets(state, quantity, ability);
            UnlessCost::Fixed {
                cost: ManaCost::generic(amount.max(0) as u32),
            }
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

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
                source_static: None,
                unless_payment: None,
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
    fn cant_be_countered_spell_stays_on_stack() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

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
                source_static: None,
                unless_payment: None,
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
        use crate::types::ability::{ContinuousModification, Duration, StaticDefinition};

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
            },
        });

        let source_static = StaticDefinition::continuous()
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::StackAbility,
                source_static: Some(source_static),
                unless_payment: None,
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
        use crate::types::ability::{ContinuousModification, StaticDefinition};

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
                source_static: Some(source_static),
                unless_payment: None,
            },
            vec![TargetRef::Object(spell_id)],
            tidebinder,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // Spell countered, but source_static should NOT be applied (it's a spell, not an ability)
        assert!(
            state.transient_continuous_effects.is_empty(),
            "source_static should not apply when countering a spell"
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
                source_static: None,
                unless_payment: None,
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

    /// CR 701.6 + CR 405.1: Mass counter iterates the stack and counters every
    /// spell matching the class filter. Mixed-population test: P1 has two
    /// spells (matched by `Card + InZone Stack`), P0 has one (excluded by
    /// the `controller: Opponent` constraint relative to P0 = ability
    /// controller). Only P1's spells should leave the stack.
    #[test]
    fn test_counter_all_opponent_spells_filters_own_spells() {
        use crate::types::ability::{ControllerRef, FilterProp, TypeFilter, TypedFilter};
        use crate::types::card_type::{CardType, CoreType};

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

        let opponent_spell_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Card],
            controller: Some(ControllerRef::Opponent),
            properties: vec![FilterProp::InZone { zone: Zone::Stack }],
        });

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
            1,
            "P0's spell remains on stack, P1's two spells countered"
        );
        assert_eq!(
            state.stack.iter().next().unwrap().id,
            p0_spell,
            "remaining stack entry is P0's own spell"
        );
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

    /// CR 113.3 + CR 405.1: "Counter all abilities" — the resolver matches
    /// every activated/triggered ability on the stack via
    /// `TargetFilter::StackAbility` and removes the entry without moving any
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

        // Two triggered abilities + one activated ability + one spell on stack.
        let trig_a = ObjectId(901);
        let trig_b = ObjectId(902);
        let act = ObjectId(903);
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
                target: TargetFilter::StackAbility,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All three abilities removed; spell remains on stack.
        assert_eq!(state.stack.len(), 1, "only the spell remains");
        assert_eq!(state.stack.iter().next().unwrap().id, spell);
        // No card moved to graveyard (abilities aren't cards).
        assert!(state.players[1].graveyard.is_empty());
        // Three SpellCountered events for the three abilities.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::SpellCountered { .. }))
                .count(),
            3
        );
    }
}

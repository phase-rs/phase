use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    CombatDamageScope, DamageTargetFilter, DamageTargetPlayerScope, Effect, EffectError,
    EffectKind, FilterProp, PreventionAmount, PreventionScope, ReplacementDefinition,
    ResolvedAbility, SubAbilityLink, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// Resolve each child of an `And`/`Or` source filter and drop `StackSpell`
/// legs (which `resolve_source_filter` maps to `TargetFilter::Any`). A leg that
/// resolves to a bare `Any` carries no damage-time constraint, so it is pruned
/// from the conjunction/disjunction — keeping only the `SpecificObject` identity
/// pin and the typed (instant/sorcery) recheck. See `resolve_source_filter`'s
/// `StackSpell` arm (CR 609.7a).
fn resolve_and_prune_stack_spell_legs(
    filters: &[TargetFilter],
    state: &GameState,
    source_id: ObjectId,
    ability_targets: &[TargetRef],
) -> Vec<TargetFilter> {
    filters
        .iter()
        .map(|inner| resolve_source_filter(inner, state, source_id, ability_targets))
        .filter(|f| !matches!(f, TargetFilter::Any))
        .collect()
}

/// CR 614.1a: Resolve a damage source filter, replacing dynamic references
/// (e.g., `IsChosenColor`, `ParentTargetSlot`) with concrete values from the
/// source object's state and the ability's chosen targets.
fn resolve_source_filter(
    filter: &TargetFilter,
    state: &GameState,
    source_id: ObjectId,
    ability_targets: &[TargetRef],
) -> TargetFilter {
    match filter {
        // CR 609.7a: a cast-time-chosen source object ("target instant or
        // sorcery spell") is captured into a SpecificObject shield so it
        // persists after the spell leaves the stack.
        TargetFilter::ParentTargetSlot { index } => ability_targets
            .get(*index)
            .and_then(|t| match t {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
            .map(|id| TargetFilter::SpecificObject { id })
            .unwrap_or(TargetFilter::None),
        TargetFilter::ChosenDamageSource => state
            .last_chosen_damage_source
            .as_ref()
            .map(|choice| TargetFilter::And {
                filters: vec![
                    TargetFilter::SpecificObject {
                        id: choice.source_id,
                    },
                    resolve_source_filter(&choice.source_filter, state, source_id, ability_targets),
                ],
            })
            .unwrap_or(TargetFilter::None),
        TargetFilter::Not { filter: inner } => TargetFilter::Not {
            filter: Box::new(resolve_source_filter(
                inner,
                state,
                source_id,
                ability_targets,
            )),
        },
        // CR 609.7a: A `StackSpell` leg ("instant or sorcery SPELL") is a
        // targeting-enumeration predicate (zone presence on the stack), not a
        // damage-time property recheck. Once the chosen source is pinned by its
        // `SpecificObject` identity, CR 609.7a applies the shield "even if that
        // object is no longer in the zone it used to be in" — and the resolving
        // spell deals its damage while leaving the stack. `matches_target_filter`
        // never matches `StackSpell` at damage time (it is handled only at
        // targeting call sites), so the leg is dropped here, leaving the typed
        // (instant/sorcery) recheck (CR 609.7b) intact.
        TargetFilter::StackSpell => TargetFilter::Any,
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: resolve_and_prune_stack_spell_legs(filters, state, source_id, ability_targets),
        },
        TargetFilter::And { filters } => {
            let pruned =
                resolve_and_prune_stack_spell_legs(filters, state, source_id, ability_targets);
            // An `And` reduced to a single non-trivial leg collapses to that leg.
            match pruned.len() {
                0 => TargetFilter::Any,
                1 => pruned.into_iter().next().unwrap(),
                _ => TargetFilter::And { filters: pruned },
            }
        }
        TargetFilter::Typed(tf) => {
            let has_chosen_ref = tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenColor));
            if !has_chosen_ref {
                return filter.clone();
            }
            // Resolve IsChosenColor → concrete HasColor using source's chosen attributes.
            let chosen_color = state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.chosen_color());
            let mut resolved = tf.clone();
            resolved
                .properties
                .retain(|p| !matches!(p, FilterProp::IsChosenColor));
            if let Some(color) = chosen_color {
                resolved.properties.push(FilterProp::HasColor { color });
            }
            TargetFilter::Typed(resolved)
        }
        _ => filter.clone(),
    }
}

fn push_player_scoped_shield(
    state: &mut GameState,
    source_id: ObjectId,
    shield: ReplacementDefinition,
) {
    let source_is_active_object = state
        .objects
        .get(&source_id)
        .is_some_and(|obj| matches!(obj.zone, Zone::Battlefield | Zone::Command));
    if source_is_active_object {
        if let Some(obj) = state.objects.get_mut(&source_id) {
            obj.replacement_definitions.push(shield);
        }
    } else {
        state.pending_damage_replacements.push(shield);
    }
}

fn player_damage_filter(player: PlayerId) -> DamageTargetFilter {
    DamageTargetFilter::Player {
        player: DamageTargetPlayerScope::Specific(player),
    }
}

fn any_player_damage_filter() -> DamageTargetFilter {
    DamageTargetFilter::Player {
        player: DamageTargetPlayerScope::Any,
    }
}

fn untargeted_damage_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Option<DamageTargetFilter> {
    match target {
        TargetFilter::Any => None,
        TargetFilter::Player => Some(any_player_damage_filter()),
        TargetFilter::SpecificPlayer { id } => Some(player_damage_filter(*id)),
        filter if filter.is_context_ref() => Some(player_damage_filter(
            super::resolve_player_for_context_ref(state, ability, filter),
        )),
        _ => None,
    }
}

/// CR 614.1a: Typed permanent recipient filters ("Dogs you control",
/// "attacking artifact creatures you control") route through the shield's
/// `valid_card` slot — the runtime matches the damage recipient object
/// against this filter. Player/context refs are handled by
/// `untargeted_damage_filter` instead.
fn typed_recipient_valid_card_filter(target: &TargetFilter) -> Option<TargetFilter> {
    match target {
        TargetFilter::Any | TargetFilter::ParentTarget => None,
        filter if filter.is_context_ref() => None,
        filter => Some(filter.clone()),
    }
}

/// CR 615: Prevent damage — creates a prevention shield on the source object.
///
/// The shield is stored as a `ReplacementDefinition` with `ShieldKind::Prevention`
/// on the source object's `replacement_definitions`. The `damage_done_applier`
/// in `replacement.rs` consumes these shields when matching `ProposedEvent::Damage`.
///
/// Follows the same lifecycle as regeneration shields:
/// 1. Created here → 2. Matched/applied in replacement pipeline → 3. Cleaned up at end of turn
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, amount_dynamic, target, scope, effect_source_filter) = match &ability.effect {
        Effect::PreventDamage {
            amount,
            amount_dynamic,
            target,
            scope,
            damage_source_filter,
        } => (
            *amount,
            amount_dynamic.clone(),
            target.clone(),
            *scope,
            damage_source_filter.clone(),
        ),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected PreventDamage effect".to_string(),
            ))
        }
    };

    // CR 609.7 + CR 609.7a: A source-scoped prevent ("prevent all damage target
    // instant or sorcery spell would deal this turn") carries its chosen source
    // object in `ability.targets[0]` via a `ParentTargetSlot` sentinel in the
    // source filter. Those targets are the damage SOURCE, not a recipient — so
    // the shield must NOT be hosted on them as a recipient object. It routes to
    // the untargeted branch (pending registry) scoped via `damage_source_filter`.
    let source_scoped_prevent = matches!(
        &effect_source_filter,
        Some(TargetFilter::And { filters })
            if filters
                .iter()
                .any(|f| matches!(f, TargetFilter::ParentTargetSlot { .. }))
    );

    // CR 615.11: A dynamic prevention amount is resolved to a concrete depletion
    // count at effect-resolution time; the Next(n) shield itself is always static.
    let amount = match amount_dynamic {
        Some(expr) => {
            let n = resolve_quantity(state, &expr, ability.controller, ability.source_id);
            PreventionAmount::Next(u32::try_from(n.max(0)).unwrap_or(0))
        }
        None => amount,
    };

    // Build the prevention shield replacement definition.
    // Note: valid_card is NOT set here — targeted shields scope via placement on the target
    // object, and global shields (pending_damage_replacements) must match any damage event.
    let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .prevention_shield(amount)
        .description("Prevent damage".to_string());

    // CR 615 + CR 614.1a: Resolve damage source filter from effect definition.
    // Filters using IsChosenColor need the chosen color resolved from the source object
    // and converted to a concrete HasColor filter for the shield.
    if let Some(src_filter) = effect_source_filter {
        let resolved_filter =
            resolve_source_filter(&src_filter, state, ability.source_id, &ability.targets);
        shield = shield.damage_source_filter(resolved_filter);
    }

    // CR 615: Scope restriction — combat damage only vs all damage
    if scope == PreventionScope::CombatDamage {
        shield = shield.combat_scope(CombatDamageScope::CombatOnly);
    }

    // CR 608.2c: When the shield is bound to a parent's chosen object target
    // (Gatta and Luzzu's `ParentTarget` referencing the chosen creature), we
    // host on the object itself and scope via `valid_card: SelfRef` — the
    // player-scoped `untargeted_damage_filter` below resolves `ParentTarget`
    // to the controller, which would mis-scope an object-shield as a
    // player-shield. Skip the player-filter inference in that case.
    let host_on_parent_target_object = matches!(target, TargetFilter::ParentTarget)
        && ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(_)));

    if !host_on_parent_target_object {
        if let Some(filter) = untargeted_damage_filter(state, ability, &target) {
            shield = shield.damage_target_filter(filter);
        } else if let Some(recipient_filter) = typed_recipient_valid_card_filter(&target) {
            shield = shield.valid_card(recipient_filter);
        }
    }

    // CR 615.5: A `ContinuationStep` rider ("prevent that damage and put that
    // many +1/+1 counters on it" — Gatta and Luzzu) fires per prevented event,
    // so it installs as the shield's `runtime_execute`. A `SequentialSibling`
    // sub is an independent instruction (CR 700.2d — a separate chosen mode of a
    // modal spell, e.g. Dromoka's Command mode 3), NOT a rider; it is resolved
    // on its own by the chain walker and must not become the shield rider.
    if let Some(sub_ability) = &ability.sub_ability {
        if sub_ability.sub_link == SubAbilityLink::ContinuationStep {
            shield = shield.runtime_execute(sub_ability.as_ref().clone());
        }
    }

    // CR 615: For targeted prevention ("prevent the next N damage to target creature"),
    // the shield lives on the TARGET object — same pattern as regeneration shields.
    // This ensures the shield is found by find_applicable_replacements() which only
    // scans Battlefield/Command zones (instants move to graveyard after resolving).
    //
    // For untargeted effects (Fog: "prevent all combat damage"), the shield lives on
    // the source permanent when possible; instant/sorcery shields that need to outlive
    // stack resolution use the game-level pending registry instead.
    //
    // CR 608.2c: When this is a sub-ability of a parent that already chose a
    // target (Gatta and Luzzu's "choose target creature ... If damage would be
    // dealt to that creature this turn, prevent that damage"), the filter is
    // `ParentTarget` — a context ref that aliases to the parent's `targets`.
    // The shield host is the chosen creature in that case, so the targeted
    // branch must also accept `ParentTarget` when `ability.targets` carries the
    // inherited parent targets.
    let host_on_targets = !source_scoped_prevent
        && !ability.targets.is_empty()
        && (!target.is_context_ref() || matches!(target, TargetFilter::ParentTarget));
    if host_on_targets {
        for selected_target in &ability.targets {
            match selected_target {
                TargetRef::Object(obj_id) => {
                    // CR 614.1a: When the shield is hosted on a specific object,
                    // scope it via `valid_card: SelfRef` so it only fires on
                    // damage to its host — not damage to any object on the
                    // battlefield. Mirrors the inline-test pattern for
                    // host-bound prevention shields (e.g., Phyrexian Hydra,
                    // Gatta and Luzzu's chosen creature).
                    let mut object_shield = shield.clone();
                    if object_shield.valid_card.is_none() {
                        object_shield.valid_card = Some(TargetFilter::SelfRef);
                    }
                    if let Some(obj) = state.objects.get_mut(obj_id) {
                        obj.replacement_definitions.push(object_shield);
                    }
                }
                TargetRef::Player(player) => {
                    // Player-targeted prevention scopes to the chosen player and
                    // persists globally when created by an instant/sorcery on the stack.
                    let player_shield = shield
                        .clone()
                        .damage_target_filter(player_damage_filter(*player));
                    push_player_scoped_shield(state, ability.source_id, player_shield);
                }
            }
        }
    } else {
        // CR 615.3: Untargeted prevention — attach to source if it's a permanent on the
        // battlefield. Instants/sorceries on the Stack will be moved to graveyard/exile
        // after resolution, so their shields must go to the global registry instead.
        // find_applicable_replacements only scans Battlefield/Command zones for
        // object-attached shields.
        let is_permanent_on_battlefield = state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield);
        if is_permanent_on_battlefield {
            if let Some(obj) = state.objects.get_mut(&ability.source_id) {
                obj.replacement_definitions.push(shield);
            }
        } else {
            // Source is on the Stack (instant/sorcery mid-resolution) or already left —
            // store in game-state-level registry so it persists until end of turn.
            state.pending_damage_replacements.push(shield);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PreventDamage,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{effects::deal_damage, zones::create_object};
    use crate::types::ability::{
        PreventionAmount, PtValue, QuantityExpr, QuantityRef, ShieldKind, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::ChosenDamageSource;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_prevent_ability(
        source: ObjectId,
        amount: PreventionAmount,
        scope: PreventionScope,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::PreventDamage {
                amount,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope,
                damage_source_filter: None,
            },
            targets,
            source,
            PlayerId(0),
        )
    }

    #[test]
    fn prevent_all_creates_shield_on_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::All,
            PreventionScope::AllDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(matches!(
            obj.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(
            obj.replacement_definitions[0].event,
            ReplacementEvent::DamageDone
        );
        assert!(!obj.replacement_definitions[0].is_consumed);
    }

    #[test]
    fn dynamic_amount_resolves_to_static_next_shield() {
        // CR 615.11: a dynamic prevention amount is resolved to a concrete
        // Next(n) depletion shield at effect-resolution time. Building-block
        // test for the amount_dynamic override path, independent of any card.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cover of Winter".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::Next(1),
                amount_dynamic: Some(QuantityExpr::Fixed { value: 4 }),
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(
            matches!(
                obj.replacement_definitions[0].shield_kind,
                ShieldKind::Prevention {
                    amount: PreventionAmount::Next(4)
                }
            ),
            "dynamic Fixed(4) should resolve to a Next(4) shield, got {:?}",
            obj.replacement_definitions[0].shield_kind
        );
    }

    #[test]
    fn chosen_damage_source_resolves_to_specific_source_and_rechecked_filter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Prevention Spell".to_string(),
            Zone::Stack,
        );
        let chosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Red Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&chosen).unwrap().color = vec![ManaColor::Red];
        let source_filter =
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor {
                    color: ManaColor::Red,
                }]),
            );
        state.last_chosen_damage_source = Some(ChosenDamageSource {
            source_id: chosen,
            source_filter: source_filter.clone(),
        });

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(TargetFilter::ChosenDamageSource),
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].damage_source_filter,
            Some(TargetFilter::And {
                filters: vec![TargetFilter::SpecificObject { id: chosen }, source_filter],
            })
        );
    }

    #[test]
    fn prevent_next_n_creates_shield_with_amount() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shield".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::Next(3),
            PreventionScope::AllDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert!(matches!(
            obj.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::Next(3)
            }
        ));
    }

    #[test]
    fn combat_damage_scope_sets_combat_only() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::All,
            PreventionScope::CombatDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert_eq!(
            obj.replacement_definitions[0].combat_scope,
            Some(CombatDamageScope::CombatOnly)
        );
    }

    #[test]
    fn prevention_shield_executes_prevented_damage_followup() {
        let mut state = GameState::new_two_player(42);
        let shield_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Inkshield".to_string(),
            Zone::Stack,
        );
        let damage_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let mut token = ResolvedAbility::new(
            Effect::Token {
                name: "Inkling".to_string(),
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Inkling".to_string()],
                colors: vec![ManaColor::White, ManaColor::Black],
                keywords: vec![Keyword::Flying],
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
            shield_source,
            PlayerId(0),
        );
        token.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        });
        let ability = make_prevent_ability(
            shield_source,
            PreventionAmount::All,
            PreventionScope::CombatDamage,
            vec![],
        )
        .sub_ability(token);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 510.2 + CR 615.13: A `Prevention::All` combat shield's rider fires
        // once per simultaneous combat-damage batch. Drive the batch primitive
        // directly (combat damage no longer routes through the per-source
        // `apply_damage_to_target` inline-rider path).
        let proposed = crate::types::proposed_event::ProposedEvent::Damage {
            source_id: damage_source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: true,
            applied: std::collections::HashSet::new(),
        };
        let (survivors, tally) = crate::game::replacement::replace_combat_damage_batch(
            &mut state,
            &mut events,
            vec![proposed],
        );
        assert_eq!(survivors, vec![None], "all 3 combat damage prevented");
        // CR 615.7: the shield aggregated 3 prevented damage.
        let total: i32 = tally.values().sum();
        assert_eq!(total, 3);

        // CR 615.5: fire the rider once against the aggregate prevented amount.
        let (rid, &prevented) = tally.iter().next().unwrap();
        let runtime = state.pending_damage_replacements[rid.index]
            .runtime_execute
            .clone()
            .unwrap();
        state.last_effect_count = Some(prevented);
        state.post_replacement_continuation =
            Some(crate::types::ability::PostReplacementContinuation::Resolved(runtime));
        let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            &mut state,
            None,
            None,
            None,
            &mut events,
        );

        assert_eq!(state.players[0].life, 20);
        let inklings = state
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.name == "Inkling")
            .count();
        assert_eq!(inklings, 3);
    }

    #[test]
    fn controller_scoped_instant_prevention_only_prevents_damage_to_controller() {
        let mut state = GameState::new_two_player(42);
        let shield_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Inkshield".to_string(),
            Zone::Stack,
        );
        let damage_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Controller,
                scope: PreventionScope::CombatDamage,
                damage_source_filter: None,
            },
            vec![],
            shield_source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].damage_target_filter,
            Some(DamageTargetFilter::Player {
                player: DamageTargetPlayerScope::Specific(PlayerId(0)),
            })
        );

        let ctx = deal_damage::DamageContext::from_source(&state, damage_source).unwrap();
        let opponent_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(1)),
            2,
            true,
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            opponent_result,
            deal_damage::DamageResult::Applied(2)
        ));
        assert_eq!(state.players[1].life, 18);

        let controller_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(0)),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            controller_result,
            deal_damage::DamageResult::Applied(0)
        ));
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn player_recipient_prevention_uses_damage_target_filter() {
        let mut state = GameState::new_two_player(42);
        let shield_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Player Shield".to_string(),
            Zone::Stack,
        );
        let damage_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Player,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
            },
            vec![],
            shield_source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let shield = &state.pending_damage_replacements[0];
        assert_eq!(
            shield.damage_target_filter,
            Some(DamageTargetFilter::Player {
                player: DamageTargetPlayerScope::Any,
            })
        );
        assert_eq!(shield.valid_card, None);

        let ctx = deal_damage::DamageContext::from_source(&state, damage_source).unwrap();
        let player_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(1)),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            player_result,
            deal_damage::DamageResult::Applied(0)
        ));
        assert_eq!(state.players[1].life, 20);

        let creature_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(creature),
            2,
            false,
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            creature_result,
            deal_damage::DamageResult::Applied(2)
        ));
        assert_eq!(state.objects.get(&creature).unwrap().damage_marked, 2);
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::All,
            PreventionScope::AllDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::PreventDamage,
                ..
            }
        )));
    }

    #[test]
    fn typed_recipient_prevention_only_blocks_matching_creatures() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let pack_leader = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pack Leader".to_string(),
            Zone::Battlefield,
        );
        let dog = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Dog".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&dog).unwrap().card_types = crate::types::card_type::CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Dog".to_string()],
        };
        let bear = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear).unwrap().card_types = crate::types::card_type::CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Bear".to_string()],
        };
        let attacker = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Typed(
                    TypedFilter::creature()
                        .with_type(TypeFilter::Subtype("Dog".into()))
                        .controller(ControllerRef::You),
                ),
                scope: PreventionScope::CombatDamage,
                damage_source_filter: None,
            },
            vec![],
            pack_leader,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let shield = &state
            .objects
            .get(&pack_leader)
            .unwrap()
            .replacement_definitions[0];
        assert_eq!(
            shield.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .with_type(TypeFilter::Subtype("Dog".into()))
                    .controller(ControllerRef::You)
            ))
        );

        let ctx = deal_damage::DamageContext::from_source(&state, attacker).unwrap();
        let dog_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(dog),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(matches!(dog_result, deal_damage::DamageResult::Applied(0)));

        let bear_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(bear),
            2,
            true,
            &mut events,
        )
        .unwrap();
        assert!(matches!(bear_result, deal_damage::DamageResult::Applied(2)));
        assert_eq!(state.objects.get(&bear).unwrap().damage_marked, 2);
    }

    /// CR 615.1a: A `Prevention { All }` shield is not depletion-based — it
    /// must remain active across multiple damage events for the rest of the
    /// turn (lifetime governed by `expiry: EndOfTurn` per CR 514.2). Without
    /// this contract the shield would prevent only the first damage event
    /// (Gatta and Luzzu's reported bug, plus latent Pariah / Phyrexian Hydra
    /// breakage). The depletion semantics of `Next(N)` are exercised by
    /// `next_n_shield_remaining_capacity` below — the orthogonal axis.
    #[test]
    fn prevention_all_shield_persists_across_multiple_damage_events() {
        use crate::types::ability::ShieldKind;
        let mut state = GameState::new_two_player(42);
        let target_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        // Gatta-and-Luzzu-shaped shield: All-prevention, EOT expiry, hosted on
        // the chosen creature (valid_card SelfRef so only damage to the host
        // fires it).
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .valid_card(TargetFilter::SelfRef)
                    .description("Persistent prevention shield".to_string()),
            );
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .replacement_definitions[0]
            .expiry = Some(crate::types::ability::RestrictionExpiry::EndOfTurn);

        // Fire three damage events back-to-back.
        let ctx = deal_damage::DamageContext::from_source(&state, damage_source).unwrap();
        for _ in 0..3 {
            let mut events = Vec::new();
            let result = deal_damage::apply_damage_to_target(
                &mut state,
                &ctx,
                TargetRef::Object(target_creature),
                4,
                false,
                &mut events,
            )
            .unwrap();
            assert!(matches!(result, deal_damage::DamageResult::Applied(0)));
        }

        // Shield must still exist and still be unconsumed — every fire was
        // absorbed without depleting the host's replacement_definitions.
        let host = state.objects.get(&target_creature).unwrap();
        assert_eq!(host.damage_marked, 0, "no damage should have been marked");
        assert_eq!(
            host.replacement_definitions.len(),
            1,
            "shield must survive: {:?}",
            host.replacement_definitions
        );
        assert!(
            !host.replacement_definitions[0].is_consumed,
            "Prevention All must not be consumed on use"
        );
        assert!(matches!(
            host.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
    }

    /// CR 615.7: `Prevention { Next(N) }` IS depletion-based — confirms the
    /// orthogonal contract still holds after the All-fix above. Each absorbed
    /// damage point reduces the shield by one; consumed shields are dropped
    /// (via `is_consumed`) once N reaches zero.
    #[test]
    fn prevention_next_n_shield_depletes_with_each_use() {
        use crate::types::ability::ShieldKind;
        let mut state = GameState::new_two_player(42);
        let target_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::Next(3))
                    .valid_card(TargetFilter::SelfRef)
                    .description("Mending Hands shield".to_string()),
            );

        let ctx = deal_damage::DamageContext::from_source(&state, damage_source).unwrap();
        // First fire: 1 damage absorbed, 2 remaining.
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(target_creature),
            1,
            false,
            &mut events,
        )
        .unwrap();
        let host = state.objects.get(&target_creature).unwrap();
        assert!(matches!(
            host.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::Next(2)
            }
        ));
        // Second fire: 2 damage absorbed, 0 remaining → consumed.
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(target_creature),
            2,
            false,
            &mut events,
        )
        .unwrap();
        let host = state.objects.get(&target_creature).unwrap();
        assert!(host.replacement_definitions[0].is_consumed);
    }

    /// CR 608.2c: When a `PreventDamage` sub-ability inherits its parent's
    /// targets via `target: ParentTarget` (Gatta and Luzzu pattern), the
    /// shield must be hosted on those inherited targets — not on the
    /// ability's own source object. This regression test fixes the case where
    /// the shield was being placed on Gatta itself instead of the chosen
    /// creature, leaving the chosen creature unprotected.
    #[test]
    fn prevent_damage_with_parent_target_hosts_shield_on_inherited_targets() {
        use crate::types::ability::ShieldKind;
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gatta and Luzzu".to_string(),
            Zone::Battlefield,
        );
        let chosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Sub-ability shape: PreventDamage with target=ParentTarget and
        // ability.targets propagated from the parent TargetOnly.
        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::ParentTarget,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
            },
            vec![TargetRef::Object(chosen)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Shield must land on the chosen creature, not on Gatta.
        let chosen_obj = state.objects.get(&chosen).unwrap();
        assert_eq!(
            chosen_obj.replacement_definitions.len(),
            1,
            "shield must be hosted on the chosen target"
        );
        assert!(matches!(
            chosen_obj.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        let source_obj = state.objects.get(&source).unwrap();
        assert!(
            source_obj.replacement_definitions.is_empty(),
            "shield must NOT land on the source — got {:?}",
            source_obj.replacement_definitions
        );
    }

    /// CR 609.7a: A source-scoped prevent's `ParentTargetSlot { 0 }` sentinel is
    /// concretized into a `SpecificObject` shield from the ability's chosen
    /// target, so the prevention persists after the spell leaves the stack. The
    /// sibling `Typed` leg survives for the CR 609.7b damage-time recheck.
    /// Mirrors `chosen_damage_source_resolves_to_specific_source_and_rechecked_filter`.
    #[test]
    fn parent_target_slot_resolves_to_specific_chosen_spell() {
        use crate::types::ability::TypeFilter;
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        let typed_leg =
            TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::AnyOf(vec![
                TypeFilter::Instant,
                TypeFilter::Sorcery,
            ])));
        let source_filter = TargetFilter::And {
            filters: vec![
                TargetFilter::ParentTargetSlot { index: 0 },
                typed_leg.clone(),
            ],
        };
        let resolved = resolve_source_filter(
            &source_filter,
            &state,
            ObjectId(99),
            &[TargetRef::Object(spell)],
        );
        assert_eq!(
            resolved,
            TargetFilter::And {
                filters: vec![TargetFilter::SpecificObject { id: spell }, typed_leg],
            },
            "ParentTargetSlot must resolve to the chosen spell's SpecificObject, keeping the Typed leg"
        );
    }

    /// CR 609.7 + CR 609.7b: A source-scoped prevent shield is restricted to the
    /// ONE chosen spell — damage from a different source (a creature trigger, as
    /// in Shalai and Hallar's "+1/+1 counter → deal damage to opponent") is NOT
    /// prevented, while damage from the chosen spell IS. This is the
    /// discriminating regression for the Dromoka's Command infinite loop.
    #[test]
    fn source_scoped_shield_only_prevents_chosen_spell_not_other_sources() {
        use crate::types::ability::TypeFilter;
        let mut state = GameState::new_two_player(42);
        // The Dromoka's Command spell on the stack chooses a spell as its source.
        let dromoka = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dromoka's Command".to_string(),
            Zone::Stack,
        );
        let chosen_spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Banefire".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&chosen_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        // An unrelated creature source (Shalai) that must NOT be shielded.
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Shalai and Hallar".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(TargetFilter::And {
                    filters: vec![
                        TargetFilter::ParentTargetSlot { index: 0 },
                        TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::AnyOf(
                            vec![TypeFilter::Instant, TypeFilter::Sorcery],
                        ))),
                    ],
                }),
            },
            vec![TargetRef::Object(chosen_spell)],
            dromoka,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The shield must be a global pending shield (the source instant leaves
        // the stack), scoped to the chosen spell — NOT hosted on the chosen
        // spell as a recipient.
        assert_eq!(
            state.pending_damage_replacements.len(),
            1,
            "source-scoped shield must go to the pending registry"
        );
        assert!(
            state
                .objects
                .get(&chosen_spell)
                .unwrap()
                .replacement_definitions
                .is_empty(),
            "shield must NOT be hosted on the chosen spell as a recipient"
        );
        let shield = &state.pending_damage_replacements[0];
        assert_eq!(
            shield.damage_source_filter,
            Some(TargetFilter::And {
                filters: vec![
                    TargetFilter::SpecificObject { id: chosen_spell },
                    TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::AnyOf(vec![
                        TypeFilter::Instant,
                        TypeFilter::Sorcery,
                    ]))),
                ],
            })
        );

        // Damage from the chosen spell IS prevented.
        let spell_ctx = deal_damage::DamageContext::from_source(&state, chosen_spell).unwrap();
        let spell_result = deal_damage::apply_damage_to_target(
            &mut state,
            &spell_ctx,
            TargetRef::Player(PlayerId(0)),
            5,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(spell_result, deal_damage::DamageResult::Applied(0)),
            "damage from the chosen spell must be prevented"
        );
        assert_eq!(state.players[0].life, 20);

        // Damage from the unrelated creature is NOT prevented (no loop).
        let creature_ctx = deal_damage::DamageContext::from_source(&state, creature).unwrap();
        let creature_result = deal_damage::apply_damage_to_target(
            &mut state,
            &creature_ctx,
            TargetRef::Player(PlayerId(0)),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(creature_result, deal_damage::DamageResult::Applied(3)),
            "damage from a non-chosen source must NOT be prevented"
        );
        assert_eq!(state.players[0].life, 17);
    }

    /// CR 615.5 + CR 700.2d: A `ContinuationStep` rider (Gatta and Luzzu) is
    /// installed as the shield's `runtime_execute`, but a `SequentialSibling`
    /// sub (Dromoka's Command mode 3's independent `PutCounter`) is NOT — it is
    /// an independent instruction resolved by the chain walker, not a rider.
    #[test]
    fn sequential_sibling_sub_is_not_installed_as_shield_rider() {
        use crate::types::ability::QuantityExpr;
        use crate::types::counter::CounterType;

        fn put_counter_sub(source: ObjectId, link: SubAbilityLink) -> ResolvedAbility {
            let mut sub = ResolvedAbility::new(
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                vec![],
                source,
                PlayerId(0),
            );
            sub.sub_link = link;
            sub
        }

        // ContinuationStep rider → installed.
        {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Gatta and Luzzu".into(),
                Zone::Battlefield,
            );
            let ability = make_prevent_ability(
                source,
                PreventionAmount::All,
                PreventionScope::AllDamage,
                vec![],
            )
            .sub_ability(put_counter_sub(source, SubAbilityLink::ContinuationStep));
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            let shield = &state.objects.get(&source).unwrap().replacement_definitions[0];
            assert!(
                shield.runtime_execute.is_some(),
                "a ContinuationStep rider must install as runtime_execute"
            );
        }

        // SequentialSibling sub → NOT installed.
        {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Dromoka's Command".into(),
                Zone::Battlefield,
            );
            let ability = make_prevent_ability(
                source,
                PreventionAmount::All,
                PreventionScope::AllDamage,
                vec![],
            )
            .sub_ability(put_counter_sub(source, SubAbilityLink::SequentialSibling));
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            let shield = &state.objects.get(&source).unwrap().replacement_definitions[0];
            assert!(
                shield.runtime_execute.is_none(),
                "a SequentialSibling sub must NOT install as runtime_execute"
            );
        }
    }
}

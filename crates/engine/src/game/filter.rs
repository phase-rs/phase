//! Typed object filter matching using TargetFilter enum.
//!
//! Replaces the Forge-style string filter parsing with typed enum matching.
//! All filter logic works against the TargetFilter enum hierarchy from types/ability.rs.

use std::collections::HashSet;

use crate::game::combat;
use crate::game::game_object::GameObject;
use crate::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
use crate::types::ability::{
    ChoiceValue, ChosenAttribute, ControllerRef, FilterProp, QuantityExpr, ResolvedAbility,
    SharedQuality, SharedQualityRelation, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::game_state::{
    CounterAddedRecord, GameState, LKISnapshot, SpellCastRecord, ZoneChangeRecord,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{EtbTapState, ProposedEvent, TokenSpec};
use crate::types::zones::Zone;

/// CR 608.2c: Resolve contextual parent-target exclusions before a mass-effect scan.
///
/// This intentionally supports only `Not(ParentTarget)` inside composite filters.
/// Positive `ParentTarget` inside `And` / `Or` remains unresolved here.
pub fn normalize_contextual_filter(
    filter: &TargetFilter,
    parent_targets: &[TargetRef],
) -> TargetFilter {
    match filter {
        TargetFilter::Not { filter: inner }
            if matches!(inner.as_ref(), TargetFilter::ParentTarget) =>
        {
            let object_ids: Vec<ObjectId> = parent_targets
                .iter()
                .filter_map(|target| match target {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            match object_ids.as_slice() {
                [] => TargetFilter::Any,
                [id] => TargetFilter::Not {
                    filter: Box::new(TargetFilter::SpecificObject { id: *id }),
                },
                _ => TargetFilter::Not {
                    filter: Box::new(TargetFilter::Or {
                        filters: object_ids
                            .into_iter()
                            .map(|id| TargetFilter::SpecificObject { id })
                            .collect(),
                    }),
                },
            }
        }
        TargetFilter::Not { filter: inner } => TargetFilter::Not {
            filter: Box::new(normalize_contextual_filter(inner, parent_targets)),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .iter()
                .map(|inner| normalize_contextual_filter(inner, parent_targets))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .iter()
                .map(|inner| normalize_contextual_filter(inner, parent_targets))
                .collect(),
        },
        _ => filter.clone(),
    }
}

/// Context bundle passed into filter evaluation.
///
/// Bundles the source object, its controller, and — when available — the resolving
/// ability, so dynamic filter thresholds (e.g. `CmcLE { value: QuantityExpr::Ref
/// { Variable("X") } }`) can resolve against `ResolvedAbility::chosen_x` and
/// `ResolvedAbility::targets`.
///
/// Construct via one of the three associated functions — don't build the struct
/// literal directly; the constructors encode the correct defaults.
pub struct FilterContext<'a> {
    pub source_id: ObjectId,
    pub source_controller: Option<PlayerId>,
    pub ability: Option<&'a ResolvedAbility>,
    /// CR 613.4c: Per-recipient binding for dynamic P/T statics whose quantity
    /// is relative to the affected object ("attached to it", "other", "shares a
    /// type with it"). The pronoun "it" refers to the per-id recipient in
    /// `apply_continuous_effect`'s loop, not necessarily the static's source.
    pub recipient_id: Option<ObjectId>,
}

impl<'a> FilterContext<'a> {
    /// Bare context: source object known, controller derived from state.
    /// Use when no activating ability is in scope (combat restrictions, layer
    /// predicates, passive trigger condition checks).
    pub fn from_source(state: &GameState, source_id: ObjectId) -> Self {
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        Self {
            source_id,
            source_controller,
            ability: None,
            recipient_id: None,
        }
    }

    /// Controller explicit (source may have left play).
    /// Use for stack-resolving effects whose source is sacrificed as a cost,
    /// replacement-effect matching, etc.
    pub fn from_source_with_controller(source_id: ObjectId, controller: PlayerId) -> Self {
        Self {
            source_id,
            source_controller: Some(controller),
            ability: None,
            recipient_id: None,
        }
    }

    /// CR 613.4c: Builder used by layer evaluation when a dynamic modification's
    /// quantity is relative to the affected object. The recipient is the
    /// per-object `id` in the affected loop (the creature being modified).
    pub fn from_source_with_recipient(
        state: &GameState,
        source_id: ObjectId,
        recipient_id: ObjectId,
    ) -> Self {
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        Self {
            source_id,
            source_controller,
            ability: None,
            recipient_id: Some(recipient_id),
        }
    }

    /// CR 107.3a + CR 601.2b: Full ability context. Dynamic thresholds
    /// (`QuantityRef::Variable { "X" }`, `TargetPower`, etc.) resolve against
    /// `chosen_x` and `targets` captured at cast time.
    pub fn from_ability(ability: &'a ResolvedAbility) -> Self {
        Self {
            source_id: ability.source_id,
            source_controller: Some(ability.controller),
            ability: Some(ability),
            recipient_id: None,
        }
    }

    /// CR 109.4: Full ability context with an explicit controller override.
    /// Use when the filter controller differs from `ability.controller`
    /// (e.g., "creature that player controls" mass-move dispatched to a target
    /// player) AND the filter still needs the resolving ability for target-
    /// inheriting predicates like `FilterProp::SameNameAsParentTarget`.
    pub fn from_ability_with_controller(
        ability: &'a ResolvedAbility,
        controller: PlayerId,
    ) -> Self {
        Self {
            source_id: ability.source_id,
            source_controller: Some(controller),
            ability: Some(ability),
            recipient_id: None,
        }
    }
}

fn scoped_player_or_controller(
    ability: Option<&ResolvedAbility>,
    source_controller: Option<PlayerId>,
) -> Option<PlayerId> {
    ability.and_then(|a| a.scoped_player).or(source_controller)
}

fn controller_ref_player(
    state: &GameState,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    ability: Option<&ResolvedAbility>,
    controller: &ControllerRef,
) -> Option<PlayerId> {
    match controller {
        ControllerRef::You => source_controller,
        ControllerRef::Opponent => None,
        ControllerRef::ScopedPlayer => scoped_player_or_controller(ability, source_controller),
        ControllerRef::TargetPlayer => ability.and_then(|a| {
            a.targets.iter().find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
        }),
        ControllerRef::DefendingPlayer => {
            crate::game::combat::defending_player_for_attacker(state, source_id)
        }
    }
}

/// Check if an object matches a typed TargetFilter against the given context.
///
/// This is the unified entry point for filter evaluation. Build a
/// [`FilterContext`] via one of its constructors, then pass it here.
pub fn matches_target_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    filter_inner(
        state,
        object_id,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
    )
}

pub fn matches_target_filter_on_battlefield_entry(
    state: &GameState,
    event: &ProposedEvent,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    match event {
        ProposedEvent::ZoneChange { object_id, to, .. } if *to == Zone::Battlefield => {
            matches_target_filter(state, *object_id, filter, ctx)
        }
        ProposedEvent::CreateToken {
            owner,
            spec,
            enter_tapped,
            ..
        } => {
            let obj = build_battlefield_entry_token_object(*owner, spec, *enter_tapped);
            filter_inner_for_object(
                state,
                &obj,
                obj.id,
                filter,
                ctx.source_id,
                ctx.source_controller,
                ctx.ability,
                ctx.recipient_id,
            )
        }
        _ => false,
    }
}

/// CR 603.10: Check whether a zone-change snapshot matches a target filter.
///
/// This is the shared past-tense matcher for zone-change events whose subject has
/// already left its original zone but must still be checked against trigger or
/// condition filters using its event-time public characteristics. The snapshot is
/// authoritative for Group 1 predicates (see `zone_change_record_matches_property`);
/// Group 2 predicates join the snapshot against the live source object.
pub fn matches_target_filter_on_zone_change_record(
    state: &GameState,
    record: &ZoneChangeRecord,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    zone_change_filter_inner(
        state,
        record,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
    )
}

/// CR 122.1 + CR 122.6: Check whether a per-turn counter-placement snapshot
/// matches a target filter using the recipient's event-time characteristics.
pub fn matches_target_filter_on_counter_added_record(
    state: &GameState,
    record: &CounterAddedRecord,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let mut obj = GameObject::new(
        record.object_id,
        CardId(0),
        record.owner,
        record.name.clone(),
        Zone::Battlefield,
    );
    obj.controller = record.controller;
    obj.power = record.power;
    obj.toughness = record.toughness;
    obj.card_types.core_types = record.core_types.clone();
    obj.card_types.subtypes = record.subtypes.clone();
    obj.card_types.supertypes = record.supertypes.clone();
    obj.mana_cost = crate::types::mana::ManaCost::generic(record.mana_value);
    obj.keywords = record.keywords.clone();
    obj.color = record.colors.clone();
    obj.counters = record.counters.clone();

    filter_inner_for_object(
        state,
        &obj,
        record.object_id,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
    )
}

/// CR 400.7 + CR 608.2c: Evaluate a target filter against last-known information.
///
/// This reuses the zone-change snapshot evaluator because both paths answer the
/// same question: did the object have the requested characteristics at the last
/// moment it existed in the relevant public zone?
pub fn matches_target_filter_on_lki_snapshot(
    state: &GameState,
    object_id: ObjectId,
    lki: &LKISnapshot,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let record = ZoneChangeRecord {
        object_id,
        name: lki.name.clone(),
        core_types: lki.card_types.clone(),
        subtypes: lki.subtypes.clone(),
        supertypes: lki.supertypes.clone(),
        keywords: lki.keywords.clone(),
        power: lki.power,
        toughness: lki.toughness,
        colors: lki.colors.clone(),
        mana_value: lki.mana_value,
        controller: lki.controller,
        owner: lki.owner,
        from_zone: None,
        to_zone: Zone::Battlefield,
        attachments: vec![],
        linked_exile_snapshot: vec![],
        is_token: false,
        combat_status: Default::default(),
    };
    matches_target_filter_on_zone_change_record(state, &record, filter, ctx)
}

/// CR 603.4 + CR 603.6 + CR 603.10: Evaluate a trigger condition whose
/// subject is the object from a zone-change event.
///
/// Enter-the-battlefield conditions evaluate the live object in the destination
/// zone. Death/leaves-the-battlefield conditions evaluate the zone-change
/// record, which carries the event-time public characteristics used for LKI.
pub fn matches_zone_change_event_object_filter(
    state: &GameState,
    event: &crate::types::events::GameEvent,
    origin: Option<Zone>,
    destination: Zone,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let crate::types::events::GameEvent::ZoneChanged {
        object_id,
        from,
        to,
        record,
    } = event
    else {
        return false;
    };

    if origin.is_some_and(|required| *from != Some(required)) || *to != destination {
        return false;
    }

    if destination == Zone::Battlefield {
        matches_target_filter(state, *object_id, filter, ctx)
    } else {
        matches_target_filter_on_zone_change_record(state, record, filter, ctx)
    }
}

fn filter_inner(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    ability: Option<&ResolvedAbility>,
    recipient_id: Option<ObjectId>,
) -> bool {
    // CR 702.26b: a phased-out permanent is treated as though it does not
    // exist. The only exception the rules allow — "rules and effects that
    // specifically mention phased-out permanents" — is extraordinarily rare
    // and handled by targeted callers that bypass this choke point; the
    // safe default here is to exclude.
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.is_phased_out() {
        return false;
    }
    filter_inner_for_object(
        state,
        obj,
        object_id,
        filter,
        source_id,
        source_controller,
        ability,
        recipient_id,
    )
}

#[allow(clippy::too_many_arguments)]
fn filter_inner_for_object(
    state: &GameState,
    obj: &GameObject,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    ability: Option<&ResolvedAbility>,
    recipient_id: Option<ObjectId>,
) -> bool {
    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::Player => false,       // Players are not objects
        TargetFilter::Controller => false,   // Controller is a player, not an object
        TargetFilter::ScopedPlayer => false, // ScopedPlayer is a player, not an object
        TargetFilter::SelfRef => object_id == source_id,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            properties,
        }) => {
            // Type filters check (all must match — conjunction)
            for tf in type_filters {
                if !type_filter_matches(tf, obj, &state.all_creature_types) {
                    return false;
                }
            }
            // Controller check
            if let Some(ctrl) = controller {
                match ctrl {
                    ControllerRef::You => {
                        if source_controller != Some(obj.controller) {
                            return false;
                        }
                    }
                    ControllerRef::Opponent => {
                        if source_controller == Some(obj.controller) {
                            return false;
                        }
                    }
                    ControllerRef::ScopedPlayer => {
                        match scoped_player_or_controller(ability, source_controller) {
                            Some(pid) if pid == obj.controller => {}
                            _ => return false,
                        }
                    }
                    // CR 109.4 + CR 115.1: "target player controls" — filter scope
                    // is the player chosen as a target of the enclosing ability.
                    // Read the first TargetRef::Player from ability.targets. Fail
                    // closed if no player target is present (the parser should
                    // surface a TargetFilter::Player slot via collect_target_slots
                    // whenever this variant appears).
                    ControllerRef::TargetPlayer => {
                        let target_player = ability.and_then(|a| {
                            a.targets.iter().find_map(|t| match t {
                                TargetRef::Player(pid) => Some(*pid),
                                TargetRef::Object(_) => None,
                            })
                        });
                        match target_player {
                            Some(pid) if pid == obj.controller => {}
                            _ => return false,
                        }
                    }
                    ControllerRef::DefendingPlayer => {
                        match crate::game::combat::defending_player_for_attacker(state, source_id) {
                            Some(pid) if pid == obj.controller => {}
                            _ => return false,
                        }
                    }
                }
            }
            // All properties must match
            let source_obj = state.objects.get(&source_id);
            let source_attached_to = source_obj.and_then(|s| s.attached_to);
            let source_is_aura =
                source_obj.is_some_and(|s| s.card_types.subtypes.iter().any(|s| s == "Aura"));
            let source_is_equipment =
                source_obj.is_some_and(|s| s.card_types.subtypes.iter().any(|s| s == "Equipment"));
            let source_chosen_creature_type =
                source_obj.and_then(|s| s.chosen_creature_type().map(|t| t.to_string()));
            let empty_attrs: Vec<crate::types::ability::ChosenAttribute> = Vec::new();
            let source_chosen_attributes = source_obj
                .map(|s| s.chosen_attributes.as_slice())
                .unwrap_or(empty_attrs.as_slice());
            let source_ctx = SourceContext {
                id: source_id,
                controller: source_controller,
                attached_to: source_attached_to,
                source_is_aura,
                source_is_equipment,
                chosen_creature_type: source_chosen_creature_type.as_deref(),
                chosen_attributes: source_chosen_attributes,
                ability,
                recipient_id,
            };
            properties
                .iter()
                .all(|p| matches_filter_prop(p, state, obj, object_id, &source_ctx))
        }
        TargetFilter::Not { filter: inner } => !filter_inner_for_object(
            state,
            obj,
            object_id,
            inner,
            source_id,
            source_controller,
            ability,
            recipient_id,
        ),
        TargetFilter::Or { filters } => filters.iter().any(|f| {
            filter_inner_for_object(
                state,
                obj,
                object_id,
                f,
                source_id,
                source_controller,
                ability,
                recipient_id,
            )
        }),
        TargetFilter::And { filters } => filters.iter().all(|f| {
            filter_inner_for_object(
                state,
                obj,
                object_id,
                f,
                source_id,
                source_controller,
                ability,
                recipient_id,
            )
        }),
        // StackAbility/StackSpell targeting is handled directly at call sites, not via filter
        TargetFilter::StackAbility | TargetFilter::StackSpell => false,
        TargetFilter::SpecificObject { id: target_id } => object_id == *target_id,
        // SpecificPlayer scopes to players, not objects — no object matches.
        TargetFilter::SpecificPlayer { .. } => false,
        TargetFilter::AttachedTo => state
            .objects
            .get(&source_id)
            .and_then(|src| src.attached_to)
            .and_then(|t| t.as_object())
            .is_some_and(|attached| attached == object_id),
        TargetFilter::LastCreated => state.last_created_token_ids.contains(&object_id),
        TargetFilter::CostPaidObject => ability
            .and_then(|ability| ability.cost_paid_object.as_ref())
            .is_some_and(|snapshot| snapshot.object_id == object_id),
        // CR 603.7: Match objects in a tracked set from the originating effect.
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .is_some_and(|set| set.contains(&object_id)),
        // CR 701.33 + CR 701.18: Intersection of a tracked set with an inner
        // type filter. Used by Zimone's Experiment to route "X cards revealed
        // this way" — the Dig resolver populates a tracked set with the kept
        // (revealed) cards; this filter restricts the target space to the
        // subset matching the inner type. `TrackedSetId(0)` is a sentinel
        // resolved to the most recent tracked set by the same binding pass
        // that handles plain `TrackedSet` continuations (see
        // `effects::delayed_trigger::bind_tracked_set_to_effect`).
        TargetFilter::TrackedSetFiltered { id, filter } => {
            let in_set = state
                .tracked_object_sets
                .get(id)
                .is_some_and(|set| set.contains(&object_id));
            in_set
                && filter_inner_for_object(
                    state,
                    obj,
                    object_id,
                    filter,
                    source_id,
                    source_controller,
                    ability,
                    recipient_id,
                )
        }
        // CR 603.10a + CR 607.2a: "cards exiled with [this object]" on a
        // leaves-the-battlefield trigger resolves from the trigger event's
        // zone-change snapshot; other contexts fall back to live exile links.
        TargetFilter::ExiledBySource => {
            crate::game::players::linked_exile_cards_for_source(state, source_id)
                .iter()
                .any(|entry| entry.exiled_id == object_id)
        }
        // CR 603.7c: Event-context references resolve to players, not objects.
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::DefendingPlayer => false,
        // ParentTarget/ParentTargetController/PostReplacementSourceController resolve
        // at resolution time, not via object matching.
        TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget => false,
        // "card with the chosen name" — match against source's ChosenAttribute::CardName.
        TargetFilter::HasChosenName => {
            let chosen_name = state.objects.get(&source_id).and_then(|obj| {
                obj.chosen_attributes.iter().find_map(|a| match a {
                    ChosenAttribute::CardName(n) => Some(n.as_str()),
                    _ => None,
                })
            });
            chosen_name.is_some_and(|name| obj.name == name)
        }
        // CR 609.7a: "the chosen source" — match the ObjectId selected by
        // the prior damage-source choice while its continuation resolves.
        TargetFilter::ChosenDamageSource => state
            .last_chosen_damage_source
            .as_ref()
            .is_some_and(|choice| choice.source_id == object_id),
        // "card named [literal]" — static name match.
        TargetFilter::Named { name } => obj.name == *name,
        // CR 400.3: Owner is a player-resolving filter (resolves to the owner of
        // source_id), meaningless as an object-matching predicate.
        TargetFilter::Owner => false,
    }
}

/// Build a synthetic `GameObject` from a `TokenSpec` for filter evaluation
/// against `CreateToken` events (tokens that don't yet exist in `state.objects`).
///
/// Uses sentinel `ObjectId(u64::MAX)` — safe for type/color/keyword filters but
/// NOT for relational filters that look up the object in `state.objects`
/// (e.g., `FilterProp::Another` will always return `false` because the sentinel
/// ID is never in the object map).
fn build_battlefield_entry_token_object(
    owner: PlayerId,
    spec: &TokenSpec,
    enter_tapped: EtbTapState,
) -> GameObject {
    let mut obj = GameObject::new(
        ObjectId(u64::MAX),
        CardId(0),
        owner,
        spec.display_name.clone(),
        Zone::Battlefield,
    );
    obj.controller = owner;
    obj.is_token = true;
    obj.power = spec.power;
    obj.toughness = spec.toughness;
    obj.base_power = spec.power;
    obj.base_toughness = spec.toughness;
    obj.card_types.core_types = spec.core_types.clone();
    obj.card_types.subtypes = spec.subtypes.clone();
    obj.card_types.supertypes = spec.supertypes.clone();
    obj.base_card_types = obj.card_types.clone();
    obj.color = spec.colors.clone();
    obj.base_color = spec.colors.clone();
    obj.keywords = spec.keywords.clone();
    obj.base_keywords = spec.keywords.clone();
    for static_def in &spec.static_abilities {
        obj.static_definitions.push(static_def.clone());
    }
    obj.tapped = enter_tapped.resolve(spec.tapped);
    obj
}

fn zone_change_filter_inner(
    state: &GameState,
    record: &ZoneChangeRecord,
    filter: &TargetFilter,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    ability: Option<&ResolvedAbility>,
) -> bool {
    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::Player => false,
        TargetFilter::Controller => false,
        TargetFilter::ScopedPlayer => false,
        TargetFilter::SelfRef => record.object_id == source_id,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            properties,
        }) => {
            if !type_filters.iter().all(|tf| {
                zone_change_record_matches_type_filter(record, tf, &state.all_creature_types)
            }) {
                return false;
            }

            if let Some(ctrl) = controller {
                match ctrl {
                    ControllerRef::You if source_controller != Some(record.controller) => {
                        return false;
                    }
                    ControllerRef::Opponent if source_controller == Some(record.controller) => {
                        return false;
                    }
                    ControllerRef::ScopedPlayer => {
                        match scoped_player_or_controller(ability, source_controller) {
                            Some(pid) if pid == record.controller => {}
                            _ => return false,
                        }
                    }
                    // CR 109.4 + CR 115.1: "target player controls" — match the
                    // record's controller against the chosen player target.
                    ControllerRef::TargetPlayer => {
                        let target_player = ability.and_then(|a| {
                            a.targets.iter().find_map(|t| match t {
                                TargetRef::Player(pid) => Some(*pid),
                                TargetRef::Object(_) => None,
                            })
                        });
                        match target_player {
                            Some(pid) if pid == record.controller => {}
                            _ => return false,
                        }
                    }
                    _ => {}
                }
            }

            let source_obj = state.objects.get(&source_id);
            let source_attached_to = source_obj.and_then(|s| s.attached_to);
            let source_is_aura =
                source_obj.is_some_and(|s| s.card_types.subtypes.iter().any(|s| s == "Aura"));
            let source_is_equipment =
                source_obj.is_some_and(|s| s.card_types.subtypes.iter().any(|s| s == "Equipment"));
            let source_chosen_creature_type =
                source_obj.and_then(|s| s.chosen_creature_type().map(|t| t.to_string()));
            let empty_attrs: Vec<crate::types::ability::ChosenAttribute> = Vec::new();
            let source_chosen_attributes = source_obj
                .map(|s| s.chosen_attributes.as_slice())
                .unwrap_or(empty_attrs.as_slice());
            let source_ctx = SourceContext {
                id: source_id,
                controller: source_controller,
                attached_to: source_attached_to,
                source_is_aura,
                source_is_equipment,
                chosen_creature_type: source_chosen_creature_type.as_deref(),
                chosen_attributes: source_chosen_attributes,
                ability,
                recipient_id: None,
            };

            properties
                .iter()
                .all(|prop| zone_change_record_matches_property(prop, state, record, &source_ctx))
        }
        TargetFilter::Not { filter: inner } => {
            !zone_change_filter_inner(state, record, inner, source_id, source_controller, ability)
        }
        TargetFilter::Or { filters } => filters.iter().any(|inner| {
            zone_change_filter_inner(state, record, inner, source_id, source_controller, ability)
        }),
        TargetFilter::And { filters } => filters.iter().all(|inner| {
            zone_change_filter_inner(state, record, inner, source_id, source_controller, ability)
        }),
        TargetFilter::SpecificObject { id } => record.object_id == *id,
        // SpecificPlayer scopes to players, not objects — a zone-change record
        // is always an object transition.
        TargetFilter::SpecificPlayer { .. } => false,
        TargetFilter::HasChosenName => {
            let chosen_name = state.objects.get(&source_id).and_then(|obj| {
                obj.chosen_attributes.iter().find_map(|a| match a {
                    ChosenAttribute::CardName(n) => Some(n.as_str()),
                    _ => None,
                })
            });
            chosen_name.is_some_and(|name| record.name == name)
        }
        TargetFilter::ChosenDamageSource => false,
        TargetFilter::Named { name } => record.name == *name,

        // CR 603.10a + CR 603.6e + CR 702.6: `AttachedTo` against a zone-change
        // record resolves via the record's `attachments` snapshot — the list of
        // objects attached to the leaving permanent at the instant before the
        // move. This covers "whenever equipped creature dies" (Skullclamp) and
        // "whenever enchanted creature dies" (Aura look-back triggers): the
        // trigger source is still on the battlefield, but SBA (CR 704.5n /
        // CR 704.5m) has already cleared its live `attached_to` pointer by the
        // time `process_triggers` runs. Matching against the snapshot is the
        // authoritative last-known-information path.
        TargetFilter::AttachedTo => record
            .attachments
            .iter()
            .any(|att| att.object_id == source_id),
        TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::StackAbility
        | TargetFilter::StackSpell
        | TargetFilter::Owner => false,
    }
}

/// CR 702.73a: Changeling subtype expansion — single authority for subtype
/// matching across all zones.
///
/// Returns `true` if either:
/// - the requested `subtype` appears literally in `subtypes` (printed or
///   layer-applied), OR
/// - `keywords` contains [`Keyword::Changeling`] AND `subtype` is a known
///   creature subtype (i.e. it appears in `all_creature_types`, the
///   game-state-wide catalog of every creature subtype seen across loaded
///   decks). The CR 205.3m gate is essential — Changeling does NOT confer
///   non-creature subtypes (artifact types like Equipment, land types like
///   Plains, enchantment types like Aura, etc.).
///
/// On-battlefield objects also benefit from layer-system post-fixup
/// (`game::layers`), which physically expands subtypes for permanents with
/// Changeling. This helper is the canonical fallback for non-battlefield
/// zones — library, hand, graveyard, exile, stack, plus zone-change snapshots
/// and spell-cast records — where the layer system does not run.
fn subtype_matches_with_changeling(
    subtype: &str,
    subtypes: &[String],
    keywords: &[Keyword],
    all_creature_types: &[String],
) -> bool {
    if subtypes.iter().any(|s| s.eq_ignore_ascii_case(subtype)) {
        return true;
    }
    // CR 702.73a: "every creature type" — gated by the CR 205.3m creature
    // subtype namespace via the runtime catalog.
    if keywords.iter().any(|k| matches!(k, Keyword::Changeling))
        && all_creature_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(subtype))
    {
        return true;
    }
    false
}

/// Check if an object matches a TypeFilter variant.
/// Check if an object's card types match a `TypeFilter`.
/// CR 205.2a: Each card type has its own rules for how it behaves.
/// Public for use by trigger_matchers and other modules that need type checking.
pub fn type_filter_matches(
    tf: &TypeFilter,
    obj: &GameObject,
    all_creature_types: &[String],
) -> bool {
    match tf {
        TypeFilter::Creature => obj.card_types.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => obj.card_types.core_types.contains(&CoreType::Land),
        // CR 301: Artifact type check.
        TypeFilter::Artifact => obj.card_types.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => obj.card_types.core_types.contains(&CoreType::Enchantment),
        // CR 304: Instant type check.
        TypeFilter::Instant => obj.card_types.core_types.contains(&CoreType::Instant),
        TypeFilter::Sorcery => obj.card_types.core_types.contains(&CoreType::Sorcery),
        // CR 306: Planeswalker type check.
        TypeFilter::Planeswalker => obj.card_types.core_types.contains(&CoreType::Planeswalker),
        // CR 310: Battle type check.
        TypeFilter::Battle => obj.card_types.core_types.contains(&CoreType::Battle),
        // CR 403.3: Permanents exist only on the battlefield — creatures, artifacts, enchantments, lands, planeswalkers, battles.
        TypeFilter::Permanent => {
            obj.card_types.core_types.contains(&CoreType::Creature)
                || obj.card_types.core_types.contains(&CoreType::Artifact)
                || obj.card_types.core_types.contains(&CoreType::Enchantment)
                || obj.card_types.core_types.contains(&CoreType::Land)
                || obj.card_types.core_types.contains(&CoreType::Planeswalker)
                || obj.card_types.core_types.contains(&CoreType::Battle)
        }
        TypeFilter::Card | TypeFilter::Any => true,
        TypeFilter::Non(inner) => !type_filter_matches(inner, obj, all_creature_types),
        // CR 205.3 + CR 702.73a: Subtype matching — battlefield layer system
        // expands Changeling into `obj.card_types.subtypes`, but for cards in
        // library/hand/graveyard/exile the helper below handles the expansion
        // by inspecting `obj.keywords` and the runtime creature-type catalog.
        TypeFilter::Subtype(ref sub) => subtype_matches_with_changeling(
            sub,
            &obj.card_types.subtypes,
            &obj.keywords,
            all_creature_types,
        ),
        // CR 608.2b: Disjunction — matches if any inner filter matches.
        TypeFilter::AnyOf(ref filters) => filters
            .iter()
            .any(|f| type_filter_matches(f, obj, all_creature_types)),
    }
}

fn zone_change_record_matches_type_filter(
    record: &ZoneChangeRecord,
    tf: &TypeFilter,
    all_creature_types: &[String],
) -> bool {
    match tf {
        TypeFilter::Creature => record.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => record.core_types.contains(&CoreType::Land),
        TypeFilter::Artifact => record.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => record.core_types.contains(&CoreType::Enchantment),
        TypeFilter::Instant => record.core_types.contains(&CoreType::Instant),
        TypeFilter::Sorcery => record.core_types.contains(&CoreType::Sorcery),
        TypeFilter::Planeswalker => record.core_types.contains(&CoreType::Planeswalker),
        TypeFilter::Battle => record.core_types.contains(&CoreType::Battle),
        TypeFilter::Permanent => {
            record.core_types.contains(&CoreType::Creature)
                || record.core_types.contains(&CoreType::Artifact)
                || record.core_types.contains(&CoreType::Enchantment)
                || record.core_types.contains(&CoreType::Land)
                || record.core_types.contains(&CoreType::Planeswalker)
                || record.core_types.contains(&CoreType::Battle)
        }
        TypeFilter::Card | TypeFilter::Any => true,
        TypeFilter::Non(inner) => {
            !zone_change_record_matches_type_filter(record, inner, all_creature_types)
        }
        // CR 205.3 + CR 702.73a: Subtype match through the Changeling helper —
        // zone-change records snapshot the object's keywords, so Changeling
        // travels with the snapshot.
        TypeFilter::Subtype(subtype) => subtype_matches_with_changeling(
            subtype,
            &record.subtypes,
            &record.keywords,
            all_creature_types,
        ),
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .any(|inner| zone_change_record_matches_type_filter(record, inner, all_creature_types)),
    }
}

/// Check whether a spell-cast history record matches a target filter.
///
/// Evaluates the subset of `TargetFilter` that is meaningful for spell snapshots.
/// Variants that only make sense for on-battlefield objects (e.g. `AttachedTo`,
/// `SpecificObject`) explicitly return `false` — no catch-all fall-through.
#[allow(clippy::only_used_in_recursion)] // controller is checked in Typed branch for Opponent
pub fn spell_record_matches_filter(
    record: &SpellCastRecord,
    filter: &TargetFilter,
    controller: PlayerId,
    all_creature_types: &[String],
) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: filter_controller,
            properties,
        }) => {
            // Spell history is already per-player, so ControllerRef::You is always
            // satisfied when we're checking spells from that player's history.
            if let Some(ctrl) = filter_controller {
                match ctrl {
                    ControllerRef::You => {}
                    ControllerRef::Opponent => return false,
                    ControllerRef::ScopedPlayer => return false,
                    // CR 109.4: A target-player-scoped filter has no meaning for
                    // a spell-history record (no ability context to resolve the
                    // target). Fail closed — this combination should not be
                    // produced by the parser.
                    ControllerRef::TargetPlayer => return false,
                    ControllerRef::DefendingPlayer => return false,
                }
            }

            type_filters.iter().all(|type_filter| {
                spell_record_matches_type_filter(record, type_filter, all_creature_types)
            }) && properties
                .iter()
                .all(|prop| spell_record_matches_property(record, prop))
        }
        TargetFilter::Or { filters } => filters.iter().any(|inner| {
            spell_record_matches_filter(record, inner, controller, all_creature_types)
        }),
        TargetFilter::And { filters } => filters.iter().all(|inner| {
            spell_record_matches_filter(record, inner, controller, all_creature_types)
        }),
        TargetFilter::Not { filter: inner } => {
            !spell_record_matches_filter(record, inner, controller, all_creature_types)
        }
        // All remaining variants are inapplicable to spell snapshots.
        TargetFilter::None
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::ScopedPlayer
        | TargetFilter::SelfRef
        | TargetFilter::StackAbility
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. }
        | TargetFilter::Owner => false,
    }
}

/// Check whether a spell object being cast matches a target filter.
///
/// Unlike [`spell_record_matches_filter`], this preserves the spell's current zone
/// and interprets `ControllerRef` relative to the current caster rather than the
/// object's stored controller.
///
/// CR 601.2a: After announcement, the spell's live `zone` is `Zone::Stack`, but
/// "spells cast from [zone]" filters on battlefield statics (CastWithKeyword,
/// ReduceCost, RaiseCost) must evaluate against the pre-announcement zone.
/// Callers inside the casting pipeline should pass `origin_zone` via
/// [`spell_object_matches_filter_from`]; this no-override helper falls back to
/// the object's current zone for legacy call sites that aren't mid-cast-aware.
pub fn spell_object_matches_filter(
    spell_obj: &GameObject,
    caster: PlayerId,
    filter: &TargetFilter,
    source_controller: PlayerId,
    all_creature_types: &[String],
) -> bool {
    spell_object_matches_filter_from(
        spell_obj,
        spell_obj.zone,
        caster,
        filter,
        source_controller,
        all_creature_types,
    )
}

/// Variant of [`spell_object_matches_filter`] that treats the spell as being
/// in `origin_zone` for filter evaluation — used during the cast pipeline where
/// the object has already physically moved to `Zone::Stack` at announcement
/// (CR 601.2a) but filters must still see the pre-announcement zone.
pub fn spell_object_matches_filter_from(
    spell_obj: &GameObject,
    origin_zone: Zone,
    caster: PlayerId,
    filter: &TargetFilter,
    source_controller: PlayerId,
    all_creature_types: &[String],
) -> bool {
    let record = spell_cast_record_from_object(spell_obj);
    spell_object_matches_filter_inner(
        &record,
        origin_zone,
        caster,
        filter,
        source_controller,
        all_creature_types,
        None,
    )
}

/// State-aware variant of [`spell_object_matches_filter_from`] for live cast
/// evaluation. Dynamic CMC thresholds on battlefield statics resolve against
/// the static source's controller and source object.
pub fn spell_object_matches_filter_from_state(
    state: &GameState,
    spell_obj: &GameObject,
    origin_zone: Zone,
    caster: PlayerId,
    filter: &TargetFilter,
    source_id: ObjectId,
    all_creature_types: &[String],
) -> bool {
    let Some(source_obj) = state.objects.get(&source_id) else {
        return false;
    };
    let record = spell_cast_record_from_object(spell_obj);
    spell_object_matches_filter_inner(
        &record,
        origin_zone,
        caster,
        filter,
        source_obj.controller,
        all_creature_types,
        Some(SpellFilterContext {
            state,
            source_id,
            source_controller: source_obj.controller,
        }),
    )
}

fn spell_cast_record_from_object(spell_obj: &GameObject) -> SpellCastRecord {
    SpellCastRecord {
        core_types: spell_obj.card_types.core_types.clone(),
        supertypes: spell_obj.card_types.supertypes.clone(),
        subtypes: spell_obj.card_types.subtypes.clone(),
        keywords: spell_obj.keywords.clone(),
        colors: spell_obj.color.clone(),
        mana_value: spell_obj.mana_cost.mana_value(),
        has_x_in_cost: crate::game::casting_costs::cost_has_x(&spell_obj.mana_cost),
    }
}

#[derive(Clone, Copy)]
struct SpellFilterContext<'a> {
    state: &'a GameState,
    source_id: ObjectId,
    source_controller: PlayerId,
}

fn spell_object_matches_filter_inner(
    record: &SpellCastRecord,
    zone: Zone,
    caster: PlayerId,
    filter: &TargetFilter,
    source_controller: PlayerId,
    all_creature_types: &[String],
    context: Option<SpellFilterContext<'_>>,
) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            properties,
        }) => {
            if let Some(ctrl) = controller {
                match ctrl {
                    ControllerRef::You if caster != source_controller => return false,
                    ControllerRef::Opponent if caster == source_controller => return false,
                    ControllerRef::ScopedPlayer => return false,
                    // CR 109.4: Target-player scope is undefined for spell-cast
                    // history (no ability context). Fail closed.
                    ControllerRef::TargetPlayer => return false,
                    ControllerRef::DefendingPlayer => return false,
                    _ => {}
                }
            }

            type_filters.iter().all(|type_filter| {
                spell_record_matches_type_filter(record, type_filter, all_creature_types)
            }) && properties.iter().all(|prop| {
                spell_object_matches_property(record, zone, prop, all_creature_types, context)
            })
        }
        TargetFilter::Or { filters } => filters.iter().any(|inner| {
            spell_object_matches_filter_inner(
                record,
                zone,
                caster,
                inner,
                source_controller,
                all_creature_types,
                context,
            )
        }),
        TargetFilter::And { filters } => filters.iter().all(|inner| {
            spell_object_matches_filter_inner(
                record,
                zone,
                caster,
                inner,
                source_controller,
                all_creature_types,
                context,
            )
        }),
        TargetFilter::Not { filter: inner } => !spell_object_matches_filter_inner(
            record,
            zone,
            caster,
            inner,
            source_controller,
            all_creature_types,
            context,
        ),
        TargetFilter::None
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::ScopedPlayer
        | TargetFilter::SelfRef
        | TargetFilter::StackAbility
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. }
        | TargetFilter::Owner => false,
    }
}

fn spell_object_matches_property(
    record: &SpellCastRecord,
    zone: Zone,
    prop: &FilterProp,
    all_creature_types: &[String],
    context: Option<SpellFilterContext<'_>>,
) -> bool {
    match prop {
        FilterProp::InZone { zone: required } => zone == *required,
        FilterProp::InAnyZone { zones } => zones.contains(&zone),
        FilterProp::Cmc { comparator, value } => {
            let threshold = match value {
                QuantityExpr::Fixed { value } => *value,
                _ => {
                    let Some(context) = context else {
                        return false;
                    };
                    resolve_quantity(
                        context.state,
                        value,
                        context.source_controller,
                        context.source_id,
                    )
                }
            };
            comparator.evaluate(record.mana_value as i32, threshold)
        }
        FilterProp::IsChosenCreatureType => context.is_some_and(|context| {
            context
                .state
                .objects
                .get(&context.source_id)
                .and_then(|source| source.chosen_creature_type())
                .is_some_and(|chosen| {
                    subtype_matches_with_changeling(
                        chosen,
                        &record.subtypes,
                        &record.keywords,
                        all_creature_types,
                    )
                })
        }),
        FilterProp::IsChosenColor => context.is_some_and(|context| {
            context
                .state
                .objects
                .get(&context.source_id)
                .and_then(|source| {
                    source.chosen_attributes.iter().find_map(|attr| match attr {
                        ChosenAttribute::Color(color) => Some(color),
                        _ => None,
                    })
                })
                .is_some_and(|color| record.colors.contains(color))
        }),
        FilterProp::IsChosenCardType => context.is_some_and(|context| {
            context
                .state
                .objects
                .get(&context.source_id)
                .and_then(|source| {
                    source.chosen_attributes.iter().find_map(|attr| match attr {
                        ChosenAttribute::CardType(card_type) => Some(card_type),
                        _ => None,
                    })
                })
                .is_some_and(|card_type| record.core_types.contains(card_type))
        }),
        _ => spell_record_matches_property(record, prop),
    }
}

fn spell_record_matches_type_filter(
    record: &SpellCastRecord,
    filter: &TypeFilter,
    all_creature_types: &[String],
) -> bool {
    match filter {
        TypeFilter::Creature => record.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => record.core_types.contains(&CoreType::Land),
        TypeFilter::Artifact => record.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => record.core_types.contains(&CoreType::Enchantment),
        TypeFilter::Instant => record.core_types.contains(&CoreType::Instant),
        TypeFilter::Sorcery => record.core_types.contains(&CoreType::Sorcery),
        TypeFilter::Planeswalker => record.core_types.contains(&CoreType::Planeswalker),
        TypeFilter::Battle => record.core_types.contains(&CoreType::Battle),
        TypeFilter::Permanent => {
            record.core_types.contains(&CoreType::Creature)
                || record.core_types.contains(&CoreType::Artifact)
                || record.core_types.contains(&CoreType::Enchantment)
                || record.core_types.contains(&CoreType::Land)
                || record.core_types.contains(&CoreType::Planeswalker)
                || record.core_types.contains(&CoreType::Battle)
        }
        TypeFilter::Card | TypeFilter::Any => true,
        TypeFilter::Non(inner) => {
            !spell_record_matches_type_filter(record, inner, all_creature_types)
        }
        // CR 205.3 + CR 702.73a: Spell-cast records snapshot keywords, so
        // Ur-Dragon's "Dragon spells you cast" matches Mistform Ultimus on the
        // stack via Changeling.
        TypeFilter::Subtype(subtype) => subtype_matches_with_changeling(
            subtype,
            &record.subtypes,
            &record.keywords,
            all_creature_types,
        ),
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .any(|inner| spell_record_matches_type_filter(record, inner, all_creature_types)),
    }
}

fn spell_record_matches_property(record: &SpellCastRecord, prop: &FilterProp) -> bool {
    match prop {
        FilterProp::WithKeyword { value } => record.keywords.iter().any(|k| k == value),
        FilterProp::HasKeywordKind { value } => record.keywords.iter().any(|k| k.kind() == *value),
        FilterProp::WithoutKeyword { value } => !record.keywords.iter().any(|k| k == value),
        FilterProp::WithoutKeywordKind { value } => {
            !record.keywords.iter().any(|k| k.kind() == *value)
        }
        FilterProp::HasColor { color } => record.colors.contains(color),
        FilterProp::NotColor { color } => !record.colors.contains(color),
        FilterProp::HasSupertype { value } => record.supertypes.contains(value),
        FilterProp::NotSupertype { value } => !record.supertypes.contains(value),
        // CR 700.6: An object is historic if it has the legendary supertype,
        // the artifact card type, or the Saga subtype. Snapshot-derivable from
        // the cast-time card-type record — used by "whenever you cast a
        // historic spell" triggers.
        FilterProp::Historic => {
            record.supertypes.contains(&Supertype::Legendary)
                || record.core_types.contains(&CoreType::Artifact)
                || record.subtypes.iter().any(|s| s == "Saga")
        }
        FilterProp::Multicolored => record.colors.len() > 1,
        // CR 105.2c: Colorless objects have no color.
        FilterProp::Colorless => record.colors.is_empty(),
        FilterProp::Cmc { comparator, value } => match value {
            QuantityExpr::Fixed { value: v } => comparator.evaluate(record.mana_value as i32, *v),
            _ => {
                debug_assert!(false, "dynamic QuantityExpr in spell record Cmc filter — parser should only produce Fixed values here");
                false
            }
        },
        // CR 202.1: Exact printed mana cost is not captured in cast-history
        // snapshots. Fail closed rather than approximating with mana value
        // (CR 202.3), which would conflate {W} with {1}.
        FilterProp::ManaCostIn { .. } => false,
        // CR 107.3 + CR 202.1: The snapshot captured whether the printed mana
        // cost contained an `{X}` shard at cast time.
        FilterProp::HasXInManaCost => record.has_x_in_cost,
        // CR 605.1: Spell-cast records snapshot the spell object, not the
        // object's ability list. Fail closed for history predicates.
        FilterProp::HasManaAbility
        // CR 113.1 + CR 113.3: Spell-cast records snapshot keywords but not
        // all ability lists, so "no abilities" cannot be proven here.
        | FilterProp::HasNoAbilities => false,
        // Disjunctive composite: recurse into inner props under the same snapshot.
        FilterProp::AnyOf { props } => props
            .iter()
            .any(|p| spell_record_matches_property(record, p)),
        // CR 111.1: Spell-cast records only track cast spells. Tokens are
        // permanents, so token identity is false and nontoken identity is true
        // for this snapshot shape.
        FilterProp::Token => false,
        FilterProp::NonToken => true,
        // All remaining props require on-battlefield or stack state unavailable from a snapshot.
        FilterProp::Attacking
        | FilterProp::AttackingController
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::Unblocked
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::CountersGE { .. }
        | FilterProp::HasAnyCounter
        | FilterProp::InZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::Another
        | FilterProp::OtherThanTriggerObject
        | FilterProp::PowerLE { .. }
        | FilterProp::PowerGE { .. }
        | FilterProp::ToughnessLE { .. }
        | FilterProp::ToughnessGE { .. }
        | FilterProp::PowerGTSource
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::HasSingleTarget
        | FilterProp::Suspected
        // CR 700.9: Modified requires on-battlefield attachments/counters,
        // unavailable from a stack-snapshot record.
        | FilterProp::Modified
        | FilterProp::ToughnessGTPower
        | FilterProp::DifferentNameFrom { .. }
        | FilterProp::InAnyZone { .. }
        | FilterProp::SharesQuality { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::AttackedThisTurn
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::FaceDown
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::NameMatchesAnyPermanent { .. }
        // CR 903.3d: Commander designation is meaningful for permanents on the
        // battlefield. The spell-cast record path is not currently plumbed with
        // commander identity — fail closed until a "cast a commander" use-case
        // requires it (CR 903.8 commander-tax tracking lives elsewhere).
        | FilterProp::IsCommander
        | FilterProp::Other { .. } => false,
    }
}

/// Context about the source of an ability, used during filter property evaluation.
struct SourceContext<'a> {
    id: ObjectId,
    controller: Option<PlayerId>,
    /// CR 303.4 + CR 301.5: Resolved host of the source's attachment, if any.
    /// Widened to `AttachTarget` so attachment-aware filter properties
    /// (`EnchantedBy`, `EquippedBy`) can route on Object vs Player. The
    /// `FilterContext` snapshot mirrors this shape — see `FilterContext`.
    attached_to: Option<crate::game::game_object::AttachTarget>,
    /// CR 301.5f + CR 303.4: Whether the source is an attachment-capable subtype.
    /// Disambiguates `attached_to == None`: an unattached Equipment/Aura matches
    /// nothing, while a non-attachment source triggers "has any" fallback semantics.
    source_is_aura: bool,
    source_is_equipment: bool,
    chosen_creature_type: Option<&'a str>,
    chosen_attributes: &'a [crate::types::ability::ChosenAttribute],
    /// CR 107.3a + CR 601.2b: The resolving ability, when one is in scope.
    /// Dynamic filter thresholds (`QuantityRef::Variable { "X" }`, `TargetPower`, etc.)
    /// resolve against this ability's `chosen_x` and `targets`. `None` for contexts
    /// without a resolving ability (combat restrictions, layer predicates); in that
    /// case, per CR 107.2, any `Variable("X")` fallback resolves to 0.
    ability: Option<&'a ResolvedAbility>,
    /// CR 613.4c: The per-object recipient of an ongoing layer evaluation, when
    /// one is bound. Used for recipient-relative quantities ("attached to it",
    /// "other", "shares a type with it"). `None` outside per-recipient contexts
    /// (e.g., target validation, spell-record matching, single-shot quantity
    /// resolution).
    recipient_id: Option<ObjectId>,
}

/// CR 201.2 + CR 400.7: Resolve the printed name of the first
/// `TargetRef::Object` in the resolving ability's targets, falling back to the
/// LKI cache when the targeted object has already left its zone (e.g. exiled
/// by the immediately preceding sub-effect).
///
/// Returns `None` when no ability is in scope, when the ability has no object
/// targets, or when the referenced object has no record in either `state.objects`
/// or `state.lki_cache`.
fn parent_target_name(state: &GameState, ability: Option<&ResolvedAbility>) -> Option<String> {
    let ability = ability?;
    let id = ability.targets.iter().find_map(|t| match t {
        crate::types::ability::TargetRef::Object(id) => Some(*id),
        crate::types::ability::TargetRef::Player(_) => None,
    })?;
    if let Some(obj) = state.objects.get(&id) {
        return Some(obj.name.clone());
    }
    state.lki_cache.get(&id).map(|lki| lki.name.clone())
}

/// Resolve a dynamic filter threshold against the source context.
///
/// When the filter evaluation has an ability in scope (e.g. SearchLibrary resolving
/// off the stack), delegate to `resolve_quantity_with_targets` so `chosen_x` and
/// targets are available. Otherwise fall back to the bare resolver (X → 0 per CR 107.2).
fn resolve_filter_threshold(
    state: &GameState,
    expr: &QuantityExpr,
    source: &SourceContext<'_>,
) -> i32 {
    match source.ability {
        Some(ability) => resolve_quantity_with_targets(state, expr, ability),
        None => resolve_quantity(
            state,
            expr,
            source.controller.unwrap_or(PlayerId(0)),
            source.id,
        ),
    }
}

fn matches_last_chosen_land_or_nonland_kind(
    choice: &Option<ChoiceValue>,
    core_types: &[CoreType],
) -> bool {
    let is_land = core_types.contains(&CoreType::Land);
    match choice {
        Some(ChoiceValue::Label(label)) if label.eq_ignore_ascii_case("Land") => is_land,
        Some(ChoiceValue::Label(label)) if label.eq_ignore_ascii_case("Nonland") => !is_land,
        _ => false,
    }
}

/// Check if an object satisfies a single FilterProp.
fn matches_filter_prop(
    prop: &FilterProp,
    state: &GameState,
    obj: &GameObject,
    object_id: ObjectId,
    source: &SourceContext<'_>,
) -> bool {
    match prop {
        // CR 111.1: Token identity of the live object.
        FilterProp::Token => state
            .objects
            .get(&object_id)
            .is_some_and(|obj| obj.is_token),
        // CR 111.1: Nontoken identity of the live object.
        FilterProp::NonToken => state
            .objects
            .get(&object_id)
            .is_some_and(|obj| !obj.is_token),
        FilterProp::Attacking => state.combat.as_ref().is_some_and(|combat| {
            combat
                .attackers
                .iter()
                .any(|attacker| attacker.object_id == object_id)
        }),
        // CR 508.1b: Matches attacking creatures whose defending player equals the
        // filter's source controller ("creatures attacking you").
        FilterProp::AttackingController => state.combat.as_ref().is_some_and(|combat| {
            combat.attackers.iter().any(|a| {
                a.object_id == object_id
                    && source.controller.is_some_and(|sc| a.defending_player == sc)
            })
        }),
        // CR 509.1a: A creature is blocking if it was declared as a blocker.
        FilterProp::Blocking => state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&object_id)),
        // CR 509.1g: A blocking creature is blocking the attacking creature it
        // was assigned to block. ExtraBlockers can allow one blocker to block
        // multiple attackers, so read the reverse map's full assignment list.
        FilterProp::BlockingSource => state.combat.as_ref().is_some_and(|combat| {
            combat
                .blocker_to_attacker
                .get(&object_id)
                .is_some_and(|attackers| attackers.contains(&source.id))
        }),
        // CR 509.1h: Unblocked = attacking creature that was never assigned blockers.
        // unblocked_attackers checks the permanent `blocked` flag, not the current blocker list.
        FilterProp::Unblocked => combat::unblocked_attackers(state).contains(&object_id),
        FilterProp::Tapped => obj.tapped,
        // CR 302.6 / CR 110.5: Untapped status as targeting qualifier.
        FilterProp::Untapped => !obj.tapped,
        FilterProp::WithKeyword { value } => obj.has_keyword(value),
        FilterProp::HasKeywordKind { value } => {
            crate::game::keywords::object_has_effective_keyword_kind(state, object_id, *value)
        }
        // CR 702: "without [keyword]" — negated keyword filter.
        FilterProp::WithoutKeyword { value } => !obj.has_keyword(value),
        FilterProp::WithoutKeywordKind { value } => {
            !crate::game::keywords::object_has_effective_keyword_kind(state, object_id, *value)
        }
        // CR 122.1: Counter count threshold. Dynamic thresholds
        // (`QuantityRef::Variable { "X" }`) resolve against the ability's
        // `chosen_x` when a `ResolvedAbility` is in scope via `FilterContext::from_ability`.
        FilterProp::CountersGE {
            counter_type,
            count,
        } => {
            let actual = obj.counters.get(counter_type).copied().unwrap_or(0) as i32;
            actual >= resolve_filter_threshold(state, count, source)
        }
        // CR 122.1: Matches any object with at least one counter of any type
        // ("creature with one or more counters on it"). Counter types are keyed
        // by CounterType; a non-zero value for ANY type satisfies the predicate.
        FilterProp::HasAnyCounter => obj.counters.values().any(|&n| n > 0),
        // CR 202.3: Mana value threshold comparisons. Dynamic thresholds
        // (`QuantityRef::Variable { "X" }`) resolve against the ability's
        // `chosen_x` when a `ResolvedAbility` is in scope via `FilterContext::from_ability`.
        FilterProp::Cmc { comparator, value } => {
            let cmc = obj.mana_cost.mana_value() as i32;
            comparator.evaluate(cmc, resolve_filter_threshold(state, value, source))
        }
        // CR 202.1: Compare exact printed mana cost, not mana value (CR 202.3).
        FilterProp::ManaCostIn { costs } => costs.iter().any(|cost| cost == &obj.mana_cost),
        // CR 702.143c-d: Foretold is a designation of a card in exile, tracked
        // directly on the object. It is not equivalent to `KeywordKind::Foretell`.
        FilterProp::Foretold => obj.foretold,
        // CR 107.3 + CR 202.1: "spell with {X} in its mana cost" — inspects the
        // printed mana cost for an `{X}` shard. Applies to spells on the stack
        // and to any live-object evaluation path (e.g. static-ability filters).
        FilterProp::HasXInManaCost => crate::game::casting_costs::cost_has_x(&obj.mana_cost),
        // CR 605.1: Delegate to the single mana-ability classifier instead of
        // duplicating the definition at the filter layer.
        FilterProp::HasManaAbility => obj
            .abilities
            .iter()
            .any(crate::game::mana_abilities::is_mana_ability),
        // CR 113.1 + CR 113.3: "no abilities" means no keyword abilities and
        // no activated, triggered, replacement, or static abilities.
        FilterProp::HasNoAbilities => object_has_no_abilities(obj),
        // CR 201.2: Name matching is exact (case-insensitive comparison).
        FilterProp::Named { name } => obj.name.eq_ignore_ascii_case(name),
        // SameName: matches objects with the same name as the tracked card from context.
        // At runtime, this checks against the source object's name (the event context card).
        FilterProp::SameName => {
            if let Some(source_obj) = state.objects.get(&source.id) {
                obj.name == source_obj.name
            } else {
                false
            }
        }
        // CR 201.2: Match objects whose name equals the resolving ability's
        // first object target (the parent target captured by the chained sub-ability).
        // Falls back to the LKI cache when the targeted object has already left its zone
        // (e.g., the seed was just exiled by the preceding effect).
        FilterProp::SameNameAsParentTarget => parent_target_name(state, source.ability)
            .is_some_and(|name| obj.name.eq_ignore_ascii_case(&name)),
        // CR 201.2 + CR 201.2a: Matches if `obj.name` equals the name of any
        // permanent on the battlefield (optionally narrowed by controller).
        // Name comparison is case-insensitive per `FilterProp::Named` /
        // `FilterProp::SameName` conventions.
        FilterProp::NameMatchesAnyPermanent { controller } => {
            let controller_pid = controller.as_ref().and_then(|c| {
                controller_ref_player(state, source.id, source.controller, source.ability, c)
            });
            state.objects.values().any(|perm| {
                if perm.zone != crate::types::zones::Zone::Battlefield {
                    return false;
                }
                let controller_ok = match (controller, controller_pid) {
                    (Some(ControllerRef::You), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::Opponent), _) => {
                        source.controller.is_some() && Some(perm.controller) != source.controller
                    }
                    (Some(ControllerRef::ScopedPlayer), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::TargetPlayer), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::DefendingPlayer), Some(pid)) => perm.controller == pid,
                    (Some(_), None) => false,
                    (None, _) => true,
                };
                controller_ok && perm.name.eq_ignore_ascii_case(&obj.name)
            })
        }
        FilterProp::InZone { zone } => obj.zone == *zone,
        FilterProp::Owned { controller } => match controller {
            ControllerRef::You => source.controller == Some(obj.owner),
            ControllerRef::Opponent => {
                source.controller.is_some() && source.controller != Some(obj.owner)
            }
            ControllerRef::ScopedPlayer => {
                scoped_player_or_controller(source.ability, source.controller)
                    .is_some_and(|pid| pid == obj.owner)
            }
            // CR 109.5: Ownership relative to a chosen target player.
            // Resolves against the first TargetRef::Player in ability.targets.
            ControllerRef::TargetPlayer => source
                .ability
                .and_then(|a| {
                    a.targets.iter().find_map(|t| match t {
                        TargetRef::Player(pid) => Some(*pid),
                        TargetRef::Object(_) => None,
                    })
                })
                .is_some_and(|pid| pid == obj.owner),
            ControllerRef::DefendingPlayer => {
                crate::game::combat::defending_player_for_attacker(state, source.id)
                    .is_some_and(|pid| pid == obj.owner)
            }
        },
        // CR 303.4 + CR 301.5f: `EnchantedBy` is source-relative when the
        // source is an Aura ("enchanted creature gets +1/+1"). When the source
        // is NOT an Aura (e.g. Hateful Eidolon's "whenever an enchanted creature
        // dies"), `FilterProp` means "has at least one Aura attached". An Aura
        // that exists but is unattached matches nothing.
        FilterProp::EnchantedBy => {
            if source.attached_to.is_some() {
                // CR 303.4: An Aura attached to a player never matches an object
                // filter ("enchanted creature"); only Object hosts qualify.
                source.attached_to.and_then(|t| t.as_object()) == Some(object_id)
            } else if source.source_is_aura {
                // CR 303.4: Unattached Aura — no creature is "enchanted by" it.
                false
            } else {
                obj.attachments.iter().any(|att_id| {
                    state
                        .objects
                        .get(att_id)
                        .is_some_and(|att| att.card_types.subtypes.iter().any(|s| s == "Aura"))
                })
            }
        }
        // CR 301.5 + CR 301.5f: Same reasoning as `EnchantedBy` — source-relative
        // for Equipment sources, falling back to "has at least one Equipment
        // attached" for non-Equipment trigger sources. Unattached Equipment
        // matches nothing.
        FilterProp::EquippedBy => {
            if source.attached_to.is_some() {
                // CR 301.5: Equipment can attach only to creatures (objects), so
                // a Player host is structurally impossible here — but routing
                // through `as_object` is the typed way to express that.
                source.attached_to.and_then(|t| t.as_object()) == Some(object_id)
            } else if source.source_is_equipment {
                // CR 301.5f: Unattached Equipment — no creature is "equipped by" it.
                false
            } else {
                obj.attachments.iter().any(|att_id| {
                    state
                        .objects
                        .get(att_id)
                        .is_some_and(|att| att.card_types.subtypes.iter().any(|s| s == "Equipment"))
                })
            }
        }
        // CR 301.5 + CR 303.4: Inverse of `EnchantedBy`/`EquippedBy` — matches
        // when THIS object is attached TO the source (`obj.attached_to ==
        // Some(source.id)`). Used for "Aura and Equipment attached to ~"
        // quantity clauses on the source object (Kellan, the Fae-Blooded).
        FilterProp::AttachedToSource => {
            obj.attached_to.and_then(|t| t.as_object()) == Some(source.id)
        }
        // CR 301.5 + CR 303.4 + CR 613.4c + CR 109.3: Anaphoric "it" referent
        // in "for each X attached to it". Two contextual referents share the
        // same parser-emitted prop:
        //
        // 1. Aura/Equipment statics ("Enchanted creature gets +N/+M for each
        //    Aura and Equipment attached to it") — "it" is the per-recipient
        //    enchanted creature, supplied via `FilterContext::recipient_id`
        //    by the layer evaluator.
        // 2. Self-source triggers ("Whenever ~ attacks, put a +1/+1 counter
        //    on it for each Equipment attached to it" — Catti-brie, Wyleth)
        //    — "it" is the trigger's source object, the same as
        //    `FilterContext::source_id`. No per-recipient binding exists at
        //    trigger resolution; the source is the only sensible referent.
        //
        // The combined rule: when a recipient is bound, use it; otherwise
        // fall back to source. This is the same semantic the parser already
        // assumed: emit `AttachedToRecipient` whenever "it" appears, and
        // resolve against whichever object is the effective subject of the
        // surrounding effect.
        FilterProp::AttachedToRecipient => {
            let referent = source.recipient_id.unwrap_or(source.id);
            obj.attached_to.and_then(|t| t.as_object()) == Some(referent)
        }
        // CR 303.4 + CR 301.5: Non-source-relative attachment predicate.
        // Matches objects that have at least one attachment of the given kind whose
        // controller satisfies the optional `ControllerRef`.
        FilterProp::HasAttachment { kind, controller } => obj.attachments.iter().any(|att_id| {
            let Some(att) = state.objects.get(att_id) else {
                return false;
            };
            let kind_matches = match kind {
                crate::types::ability::AttachmentKind::Aura => {
                    att.card_types.subtypes.iter().any(|s| s == "Aura")
                }
                crate::types::ability::AttachmentKind::Equipment => {
                    att.card_types.subtypes.iter().any(|s| s == "Equipment")
                }
            };
            if !kind_matches {
                return false;
            }
            attachment_controller_matches(controller.as_ref(), att.controller, state, source)
        }),
        // CR 303.4 + CR 301.5: Disjunctive attachment predicate — matches when the
        // object has at least one attachment whose subtype is in `kinds` and whose
        // controller satisfies the optional `ControllerRef`. Generalization of
        // `HasAttachment` to the "enchanted or equipped" compound-subject class.
        FilterProp::HasAnyAttachmentOf { kinds, controller } => {
            obj.attachments.iter().any(|att_id| {
                let Some(att) = state.objects.get(att_id) else {
                    return false;
                };
                let kind_matches = kinds.iter().any(|kind| match kind {
                    crate::types::ability::AttachmentKind::Aura => {
                        att.card_types.subtypes.iter().any(|s| s == "Aura")
                    }
                    crate::types::ability::AttachmentKind::Equipment => {
                        att.card_types.subtypes.iter().any(|s| s == "Equipment")
                    }
                });
                if !kind_matches {
                    return false;
                }
                attachment_controller_matches(controller.as_ref(), att.controller, state, source)
            })
        }
        // CR 613.4c: In per-recipient layer contexts, "other" is relative to
        // the affected object. Outside those contexts, it remains source-relative.
        FilterProp::Another => object_id != source.recipient_id.unwrap_or(source.id),
        // CR 603.4 + CR 109.3: `OtherThanTriggerObject` is a typed marker that
        // signals "exclude the triggering object" for count semantics. The
        // exclusion is applied at the `QuantityRef::ObjectCount` resolver level
        // (see `game::quantity`) using the current trigger event, not here —
        // this variant acts as a transparent pass-through for per-object
        // filter evaluation so that the marker does not spuriously exclude
        // every object from individual match checks.
        FilterProp::OtherThanTriggerObject => true,
        FilterProp::HasColor { color } => obj.color.contains(color),
        // CR 208.1: Power comparison against a dynamic threshold. Dynamic thresholds
        // (`QuantityRef::Variable { "X" }`) resolve against the ability's `chosen_x`
        // when a `ResolvedAbility` is in scope via `FilterContext::from_ability`.
        FilterProp::PowerLE { value } => {
            obj.power.unwrap_or(0) <= resolve_filter_threshold(state, value, source)
        }
        FilterProp::PowerGE { value } => {
            obj.power.unwrap_or(0) >= resolve_filter_threshold(state, value, source)
        }
        // CR 208.1: Toughness comparison against a dynamic threshold.
        FilterProp::ToughnessLE { value } => {
            obj.toughness.unwrap_or(0) <= resolve_filter_threshold(state, value, source)
        }
        FilterProp::ToughnessGE { value } => {
            obj.toughness.unwrap_or(0) >= resolve_filter_threshold(state, value, source)
        }
        // Disjunctive composite: any inner prop matches.
        FilterProp::AnyOf { props } => props
            .iter()
            .any(|p| matches_filter_prop(p, state, obj, object_id, source)),
        // CR 509.1b: Object's power is strictly greater than the source object's power.
        FilterProp::PowerGTSource => {
            let source_power = state
                .objects
                .get(&source.id)
                .and_then(|o| o.power)
                .unwrap_or(0);
            obj.power.unwrap_or(0) > source_power
        }
        FilterProp::Multicolored => obj.color.len() > 1,
        // CR 105.2c: Colorless objects have no color.
        FilterProp::Colorless => obj.color.is_empty(),
        FilterProp::HasSupertype { value } => obj.card_types.supertypes.contains(value),
        // CR 205.4b: Object does NOT have this color.
        FilterProp::NotColor { color } => !obj.color.contains(color),
        // CR 205.4a: Object does NOT have this supertype.
        FilterProp::NotSupertype { value } => !obj.card_types.supertypes.contains(value),
        FilterProp::IsChosenCreatureType => match source.chosen_creature_type {
            Some(chosen) => obj
                .card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case(chosen)),
            None => false,
        },
        // CR 105.4: Match objects whose colors include the source's chosen color.
        // Used for "of the chosen color" (Hall of Triumph, Prismatic Strands).
        FilterProp::IsChosenColor => source
            .chosen_attributes
            .iter()
            .find_map(|a| match a {
                crate::types::ability::ChosenAttribute::Color(c) => Some(c),
                _ => None,
            })
            .is_some_and(|chosen| obj.color.contains(chosen)),
        // CR 205: Match objects whose core type includes the source's chosen card type.
        // Used for "spells of the chosen type" (Archon of Valor's Reach).
        FilterProp::IsChosenCardType => source
            .chosen_attributes
            .iter()
            .find_map(|a| match a {
                crate::types::ability::ChosenAttribute::CardType(ct) => Some(ct),
                _ => None,
            })
            .is_some_and(|chosen| obj.card_types.core_types.contains(chosen)),
        FilterProp::IsChosenLandOrNonlandKind => matches_last_chosen_land_or_nonland_kind(
            &state.last_named_choice,
            &obj.card_types.core_types,
        ),
        // CR 701.60b: Match creatures with the suspected designation.
        FilterProp::Suspected => obj.is_suspected,
        // CR 700.9: A permanent is modified if it has one or more counters on
        // it (CR 122), is equipped (CR 301.5), or is enchanted by an Aura
        // controlled by its controller (CR 303.4).
        FilterProp::Modified => {
            let has_counter = obj.counters.values().any(|&n| n > 0);
            let has_qualifying_attachment = obj.attachments.iter().any(|att_id| {
                let Some(att) = state.objects.get(att_id) else {
                    return false;
                };
                let is_equipment = att.card_types.subtypes.iter().any(|s| s == "Equipment");
                if is_equipment {
                    // CR 301.5: Equipment attachment alone is sufficient — no
                    // controller constraint (a creature equipped by anyone's
                    // Equipment is modified).
                    return true;
                }
                let is_aura = att.card_types.subtypes.iter().any(|s| s == "Aura");
                // CR 303.4: Aura counts only if controlled by the permanent's
                // controller.
                is_aura && att.controller == obj.controller
            });
            has_counter || has_qualifying_attachment
        }
        // CR 700.6: An object is historic if it has the legendary supertype,
        // the artifact card type, or the Saga subtype.
        FilterProp::Historic => {
            obj.card_types.supertypes.contains(&Supertype::Legendary)
                || obj.card_types.core_types.contains(&CoreType::Artifact)
                || obj.card_types.subtypes.iter().any(|s| s == "Saga")
        }
        // CR 510.1c: Match creatures whose toughness exceeds their power.
        FilterProp::ToughnessGTPower => {
            let power = obj.power.unwrap_or(0);
            let toughness = obj.toughness.unwrap_or(0);
            toughness > power
        }
        // Match objects whose name differs from all controlled battlefield objects matching the filter.
        FilterProp::DifferentNameFrom { filter } => {
            let controller = source.controller.unwrap_or(PlayerId(0));
            let nested_ctx = FilterContext::from_source_with_controller(source.id, controller);
            let controlled_names: Vec<&str> = state
                .battlefield
                .iter()
                .filter_map(|&bid| state.objects.get(&bid))
                .filter(|bobj| bobj.controller == controller)
                .filter(|bobj| matches_target_filter(state, bobj.id, filter, &nested_ctx))
                .map(|bobj| bobj.name.as_str())
                .collect();
            !controlled_names.contains(&obj.name.as_str())
        }
        // CR 604.3: Match objects in any of the listed zones (OR semantics).
        FilterProp::InAnyZone { zones } => zones.contains(&obj.zone),
        FilterProp::SharesQuality {
            quality,
            reference,
            relation,
        } => {
            let shares = reference.as_ref().is_none_or(|reference_filter| {
                object_shares_quality_with_reference_filter(
                    state,
                    obj,
                    quality,
                    reference_filter,
                    source,
                )
            });
            match relation {
                SharedQualityRelation::Shares => shares,
                SharedQualityRelation::DoesNotShare => {
                    !shares
                        && (!matches!(quality, SharedQuality::Name)
                            || !object_shared_quality_values(
                                obj,
                                quality,
                                &state.all_creature_types,
                            )
                            .is_empty())
                }
            }
        }
        // CR 510.1: Object was dealt damage this turn (damage_marked persists until cleanup).
        FilterProp::WasDealtDamageThisTurn => obj.damage_marked > 0,
        // CR 400.7: Object entered the battlefield this turn.
        FilterProp::EnteredThisTurn => obj.entered_battlefield_turn == Some(state.turn_number),
        // CR 508.1a: Creature was declared as an attacker this turn.
        FilterProp::AttackedThisTurn => state.creatures_attacked_this_turn.contains(&object_id),
        // CR 509.1a: Creature was declared as a blocker this turn.
        FilterProp::BlockedThisTurn => state.creatures_blocked_this_turn.contains(&object_id),
        // CR 508.1a + CR 509.1a: Creature attacked or blocked this turn.
        FilterProp::AttackedOrBlockedThisTurn => {
            state.creatures_attacked_this_turn.contains(&object_id)
                || state.creatures_blocked_this_turn.contains(&object_id)
        }
        // CR 115.7: Stack entry has exactly one target — permissive at filter level,
        // validated by retarget effects at resolution time.
        FilterProp::HasSingleTarget => true,
        // CR 115.9c: Stack entry's targets all match the inner filter — permissive at
        // per-object level, validated by trigger matchers and retarget effects against the
        // stack entry's actual targets.
        // CR 707.2: Match face-down permanents on the battlefield.
        FilterProp::FaceDown => obj.face_down,
        FilterProp::TargetsOnly { .. } => true,
        // CR 115.9b: Permissive at per-object level; validated by trigger matchers against
        // the stack entry's actual targets.
        FilterProp::Targets { .. } => true,
        // CR 903.3d: "If an effect refers to controlling a commander, it refers
        // to a permanent on the battlefield that is a commander." `is_commander`
        // is the deck-construction designation per CR 903.3.
        FilterProp::IsCommander => obj.is_commander,
        FilterProp::Other { .. } => false, // Fail-closed for unrecognized properties
    }
}

fn object_has_no_abilities(obj: &GameObject) -> bool {
    obj.keywords.is_empty()
        && obj.abilities.is_empty()
        && obj.trigger_definitions.is_empty()
        && obj.replacement_definitions.is_empty()
        && obj.static_definitions.is_empty()
}

/// CR 603.10: Evaluate a `FilterProp` against a zone-change event snapshot.
///
/// Properties fall into four groups:
/// 1. **Snapshot-derivable.** Read directly from the captured record — P/T, colors, CMC,
///    keywords, supertypes, types, owner/controller, name.
/// 2. **Source/event relational.** Compare the record against the source object or its
///    chosen attributes — `Another`, `Owned`, `IsChosenCreatureType`, `Named`.
/// 3. **Combat snapshot state.** Attacking/blocking/unblocked predicates read
///    `ZoneChangeRecord::combat_status`, because leaving a zone removes the
///    object from live combat.
/// 4. **Dynamic battlefield state.** Inherently requires the live object (tapped,
///    counters, attached-to). A zone-change subject has already left its public
///    zone, so these are semantically not applicable and return `false`.
/// 5. **Not-yet-supported.** Could plausibly be snapshotted or cross-referenced but
///    are not currently required. Returning `false` is a known conservative gap.
fn zone_change_record_matches_property(
    prop: &FilterProp,
    state: &GameState,
    record: &ZoneChangeRecord,
    source: &SourceContext<'_>,
) -> bool {
    match prop {
        // -------- Group 1: snapshot-derivable --------
        // CR 702: Keyword presence on the event-time object.
        FilterProp::WithKeyword { value } => record.keywords.iter().any(|k| k == value),
        FilterProp::HasKeywordKind { value } => record.keywords.iter().any(|k| k.kind() == *value),
        FilterProp::WithoutKeyword { value } => !record.keywords.iter().any(|k| k == value),
        FilterProp::WithoutKeywordKind { value } => {
            !record.keywords.iter().any(|k| k.kind() == *value)
        }
        // CR 205.4a: Supertype membership as of the zone change.
        FilterProp::HasSupertype { value } => record.supertypes.contains(value),
        FilterProp::NotSupertype { value } => !record.supertypes.contains(value),
        // CR 700.6: An object is historic if it has the legendary supertype,
        // the artifact card type, or the Saga subtype. Snapshot-derivable from
        // the zone-change card-type record — used by ETB triggers on
        // "another nontoken historic permanent you control" (Arbaaz Mir).
        FilterProp::Historic => {
            record.supertypes.contains(&Supertype::Legendary)
                || record.core_types.contains(&CoreType::Artifact)
                || record.subtypes.iter().any(|s| s == "Saga")
        }
        // CR 201.2: Name match (case-insensitive) on the event-time object.
        FilterProp::Named { name } => record.name.eq_ignore_ascii_case(name),
        // CR 208.1: Power threshold on the event-time object. A `None` power
        // (non-creature in some zones) treats as 0 — matches live-state behavior.
        FilterProp::PowerLE { value } => {
            record.power.unwrap_or(0) <= resolve_filter_threshold(state, value, source)
        }
        FilterProp::PowerGE { value } => {
            record.power.unwrap_or(0) >= resolve_filter_threshold(state, value, source)
        }
        // CR 208.1: Toughness threshold on the event-time object.
        FilterProp::ToughnessLE { value } => {
            record.toughness.unwrap_or(0) <= resolve_filter_threshold(state, value, source)
        }
        FilterProp::ToughnessGE { value } => {
            record.toughness.unwrap_or(0) >= resolve_filter_threshold(state, value, source)
        }
        // CR 202.3: Mana value threshold on the event-time object.
        FilterProp::Cmc { comparator, value } => comparator.evaluate(
            record.mana_value as i32,
            resolve_filter_threshold(state, value, source),
        ),
        // CR 202.1: Zone-change records currently snapshot mana value, not the
        // full printed mana cost. Exact-cost predicates fail closed here.
        FilterProp::ManaCostIn { .. } => false,
        // CR 105.1 / CR 202.2: Color membership on the event-time object.
        FilterProp::HasColor { color } => record.colors.contains(color),
        FilterProp::NotColor { color } => !record.colors.contains(color),
        FilterProp::Multicolored => record.colors.len() > 1,
        FilterProp::Colorless => record.colors.is_empty(),
        // CR 208.1 / CR 107.2: `toughness > power` comparison on the snapshot.
        FilterProp::ToughnessGTPower => record.toughness.unwrap_or(0) > record.power.unwrap_or(0),
        // CR 111.1: Token identity as of the zone change. Token-ness is a
        // stable property of the object, captured in the snapshot so that
        // "whenever a creature token dies" (Grismold) and similar LTB
        // triggers evaluate correctly after the token has moved to the
        // graveyard (and then ceased to exist per CR 111.7).
        FilterProp::Token => record.is_token,
        // CR 111.1 + CR 603.6a: Nontoken identity as of the zone change.
        FilterProp::NonToken => !record.is_token,

        // -------- Group 2: source/event relational --------
        // CR 109.1 "another": same-object check against the triggering source.
        FilterProp::Another => record.object_id != source.id,
        // CR 603.4 + CR 109.3: Record-variant of OtherThanTriggerObject. See the
        // comment in `matches_property_typed` — the exclusion is applied at the
        // quantity-resolver layer; here the prop is a transparent pass-through.
        FilterProp::OtherThanTriggerObject => true,
        // CR 400.1: "from [zone]" — the record's origin zone.
        // CR 111.1 + CR 603.6a: Token creation produces `from_zone = None`,
        // which cannot match any specific origin zone — correct for triggers
        // like "from the graveyard" that must not fire on tokens.
        FilterProp::InZone { zone } => record.from_zone == Some(*zone),
        // CR 109.5: Ownership relative to the source's controller.
        FilterProp::Owned { controller } => match controller {
            ControllerRef::You => source.controller == Some(record.owner),
            ControllerRef::Opponent => {
                source.controller.is_some() && source.controller != Some(record.owner)
            }
            ControllerRef::ScopedPlayer => {
                scoped_player_or_controller(source.ability, source.controller)
                    .is_some_and(|pid| pid == record.owner)
            }
            // CR 109.5: Ownership relative to a chosen target player.
            ControllerRef::TargetPlayer => source
                .ability
                .and_then(|a| {
                    a.targets.iter().find_map(|t| match t {
                        TargetRef::Player(pid) => Some(*pid),
                        TargetRef::Object(_) => None,
                    })
                })
                .is_some_and(|pid| pid == record.owner),
            ControllerRef::DefendingPlayer => {
                crate::game::combat::defending_player_for_attacker(state, source.id)
                    .is_some_and(|pid| pid == record.owner)
            }
        },
        // CR 701.12: Source's chosen creature type applied to the snapshot subtypes.
        FilterProp::IsChosenCreatureType => source.chosen_creature_type.is_some_and(|chosen| {
            record
                .subtypes
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(chosen))
        }),
        // CR 509.1b: Power comparison against the live source.
        FilterProp::PowerGTSource => {
            let source_power = state
                .objects
                .get(&source.id)
                .and_then(|o| o.power)
                .unwrap_or(0);
            record.power.unwrap_or(0) > source_power
        }
        // CR 201.2: Same-name match against the tracked source object.
        FilterProp::SameName => state
            .objects
            .get(&source.id)
            .is_some_and(|s| s.name.eq_ignore_ascii_case(&record.name)),
        // CR 201.2: Same-name match against the resolving ability's first object
        // target (parent target). Mirrors the live-object evaluator.
        FilterProp::SameNameAsParentTarget => parent_target_name(state, source.ability)
            .is_some_and(|name| record.name.eq_ignore_ascii_case(&name)),

        // -------- Group 3: combat snapshot state --------
        // CR 508.1k / CR 509.1g / CR 509.1h: Combat state as of the zone change.
        // Live combat maps are cleared when an object leaves combat (CR 506.4),
        // so look-back filters must read the zone-change snapshot.
        FilterProp::Attacking => record.combat_status.attacking,
        FilterProp::AttackingController => {
            record.combat_status.attacking
                && source.controller == record.combat_status.defending_player
        }
        FilterProp::Blocking => record.combat_status.blocking,
        // `ZoneChangeCombatStatus` snapshots role, not the blocker-to-attacker
        // relation. Source-relative blocker checks require live combat state.
        FilterProp::BlockingSource => false,
        FilterProp::Unblocked => {
            record.combat_status.attacking && !record.combat_status.blocked
        }
        FilterProp::HasAttachment { kind, controller } => record.attachments.iter().any(|att| {
            att.kind == *kind
                && attachment_controller_matches(
                    controller.as_ref(),
                    att.controller,
                    state,
                    source,
                )
        }),
        FilterProp::HasAnyAttachmentOf { kinds, controller } => {
            record.attachments.iter().any(|att| {
                kinds.contains(&att.kind)
                    && attachment_controller_matches(
                        controller.as_ref(),
                        att.controller,
                        state,
                        source,
                    )
            })
        }

        // These predicates query live battlefield state (tap status, attachment,
        // current counters, face-down). The snapshot has already left its public
        // zone, so the predicate is semantically not applicable.
        FilterProp::CountersGE {
            counter_type,
            count,
        } => state.lki_cache.get(&record.object_id).is_some_and(|lki| {
            let actual = lki.counters.get(counter_type).copied().unwrap_or(0) as i32;
            actual >= resolve_filter_threshold(state, count, source)
        }),
        FilterProp::HasAnyCounter => state
            .lki_cache
            .get(&record.object_id)
            .is_some_and(|lki| lki.counters.values().any(|&count| count > 0)),
        FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::AttackedThisTurn
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::FaceDown
        | FilterProp::Foretold
        // CR 201.2: Name-matches-any-permanent is a live-battlefield predicate
        // — a zone-change snapshot cannot represent it. Fail closed.
        | FilterProp::NameMatchesAnyPermanent { .. } => false,

        // Disjunctive composite: recurse into inner props under the same record.
        FilterProp::AnyOf { props } => props
            .iter()
            .any(|p| zone_change_record_matches_property(p, state, record, source)),

        // -------- Group 4: not-yet-supported (known conservative gaps) --------
        // These could be snapshotted (e.g. suspected status, damage-dealt-this-turn)
        // or require state joins that aren't plumbed to this evaluator. Expand as
        // trigger-filter coverage grows.
        FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::HasSingleTarget
        | FilterProp::Suspected
        // CR 700.9: Modified is a live-battlefield predicate (counters +
        // attachments) — a zone-change snapshot cannot represent it.
        | FilterProp::Modified
        | FilterProp::DifferentNameFrom { .. }
        | FilterProp::InAnyZone { .. }
        | FilterProp::SharesQuality { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        // CR 107.3 + CR 202.1: X-in-cost is a spell-cast-time predicate; it has no
        // meaning for a zone-change record (the object has already left the stack
        // or never was a spell). Fail closed — the snapshot carries no such info.
        | FilterProp::HasXInManaCost
        // CR 605.1: Zone-change records do not snapshot ability lists.
        | FilterProp::HasManaAbility
        // CR 113.1 + CR 113.3: Zone-change records do not snapshot all
        // ability lists, so "no abilities" cannot be proven here.
        | FilterProp::HasNoAbilities
        // CR 903.3d + CR 903.3: Commander designation is preserved across zones,
        // but zone-change records do not carry it. Fail closed — zone-change
        // triggers that need to filter by commander status will require record
        // plumbing (no current consumer).
        | FilterProp::IsCommander
        | FilterProp::Other { .. } => false,
    }
}

fn attachment_controller_matches(
    controller: Option<&ControllerRef>,
    attachment_controller: PlayerId,
    state: &GameState,
    source: &SourceContext<'_>,
) -> bool {
    match controller {
        None => true,
        Some(ControllerRef::You) => source.controller == Some(attachment_controller),
        Some(ControllerRef::Opponent) => source
            .controller
            .is_some_and(|controller| controller != attachment_controller),
        Some(ControllerRef::ScopedPlayer) => {
            scoped_player_or_controller(source.ability, source.controller)
                .is_some_and(|pid| pid == attachment_controller)
        }
        Some(ControllerRef::TargetPlayer) => source
            .ability
            .and_then(|a| {
                a.targets.iter().find_map(|t| match t {
                    TargetRef::Player(pid) => Some(*pid),
                    TargetRef::Object(_) => None,
                })
            })
            .is_some_and(|pid| pid == attachment_controller),
        Some(ControllerRef::DefendingPlayer) => {
            combat::defending_player_for_attacker(state, source.id)
                .is_some_and(|pid| pid == attachment_controller)
        }
    }
}

const LAND_TYPES: &[&str] = &[
    "Cave",
    "Desert",
    "Forest",
    "Gate",
    "Island",
    "Lair",
    "Locus",
    "Mine",
    "Mountain",
    "Plains",
    "Planet",
    "Power-Plant",
    "Sphere",
    "Swamp",
    "Tower",
    "Town",
    "Urza's",
];

fn is_land_type(subtype: &str) -> bool {
    LAND_TYPES
        .iter()
        .any(|land_type| subtype.eq_ignore_ascii_case(land_type))
}

fn shared_quality_values(
    name: &str,
    core_types: &[CoreType],
    subtypes: &[String],
    colors: &[ManaColor],
    keywords: &[Keyword],
    quality: &SharedQuality,
    all_creature_types: &[String],
) -> HashSet<String> {
    match quality {
        SharedQuality::Name => {
            if name.is_empty() {
                HashSet::new()
            } else {
                HashSet::from([name.to_ascii_lowercase()])
            }
        }
        SharedQuality::CreatureType => {
            if keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Changeling))
            {
                return all_creature_types
                    .iter()
                    .map(|creature_type| creature_type.to_ascii_lowercase())
                    .collect();
            }

            subtypes
                .iter()
                .filter(|subtype| {
                    all_creature_types
                        .iter()
                        .any(|creature_type| subtype.eq_ignore_ascii_case(creature_type))
                })
                .map(|subtype| subtype.to_ascii_lowercase())
                .collect()
        }
        SharedQuality::Color => colors
            .iter()
            .map(|color| format!("{color:?}").to_ascii_lowercase())
            .collect(),
        SharedQuality::CardType => core_types
            .iter()
            .map(|card_type| format!("{card_type:?}").to_ascii_lowercase())
            .collect(),
        SharedQuality::LandType => subtypes
            .iter()
            .filter(|subtype| is_land_type(subtype))
            .map(|subtype| subtype.to_ascii_lowercase())
            .collect(),
    }
}

fn object_shared_quality_values(
    obj: &GameObject,
    quality: &SharedQuality,
    all_creature_types: &[String],
) -> HashSet<String> {
    shared_quality_values(
        &obj.name,
        &obj.card_types.core_types,
        &obj.card_types.subtypes,
        &obj.color,
        &obj.keywords,
        quality,
        all_creature_types,
    )
}

fn lki_shared_quality_values(
    lki: &LKISnapshot,
    quality: &SharedQuality,
    all_creature_types: &[String],
) -> HashSet<String> {
    shared_quality_values(
        &lki.name,
        &lki.card_types,
        &lki.subtypes,
        &lki.colors,
        &lki.keywords,
        quality,
        all_creature_types,
    )
}

fn quality_sets_overlap(left: &HashSet<String>, right: &HashSet<String>) -> bool {
    !left.is_empty() && !right.is_empty() && !left.is_disjoint(right)
}

fn object_shares_quality_values(
    obj: &GameObject,
    quality: &SharedQuality,
    values: &HashSet<String>,
    all_creature_types: &[String],
) -> bool {
    quality_sets_overlap(
        &object_shared_quality_values(obj, quality, all_creature_types),
        values,
    )
}

fn parent_target_shared_quality_values(
    state: &GameState,
    source: &SourceContext<'_>,
    quality: &SharedQuality,
) -> Option<HashSet<String>> {
    // `ParentTarget` normally references the first selected object target.
    // In layer evaluation there is no selected target, so recipient-relative
    // quantities bind it to the affected object instead.
    let target_id = source
        .ability
        .and_then(|ability| {
            ability.targets.iter().find_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
        })
        .or(source.recipient_id)?;

    if let Some(obj) = state.objects.get(&target_id) {
        return Some(object_shared_quality_values(
            obj,
            quality,
            &state.all_creature_types,
        ));
    }

    state
        .lki_cache
        .get(&target_id)
        .map(|lki| lki_shared_quality_values(lki, quality, &state.all_creature_types))
}

fn object_shares_quality_with_reference_filter(
    state: &GameState,
    obj: &GameObject,
    quality: &SharedQuality,
    reference_filter: &TargetFilter,
    source: &SourceContext<'_>,
) -> bool {
    if matches!(reference_filter, TargetFilter::ParentTarget) {
        return parent_target_shared_quality_values(state, source, quality).is_some_and(|values| {
            object_shares_quality_values(obj, quality, &values, &state.all_creature_types)
        });
    }

    let event_context_references =
        crate::game::targeting::resolve_event_context_targets(state, reference_filter, source.id);
    if !event_context_references.is_empty() {
        return event_context_references
            .into_iter()
            .filter_map(|target| match target {
                TargetRef::Object(reference_id) => state.objects.get(&reference_id),
                TargetRef::Player(_) => None,
            })
            .any(|reference_obj| {
                let values =
                    object_shared_quality_values(reference_obj, quality, &state.all_creature_types);
                object_shares_quality_values(obj, quality, &values, &state.all_creature_types)
            });
    }

    state.objects.keys().copied().any(|reference_id| {
        filter_inner(
            state,
            reference_id,
            reference_filter,
            source.id,
            source.controller,
            source.ability,
            source.recipient_id,
        ) && state
            .objects
            .get(&reference_id)
            .is_some_and(|reference_obj| {
                let values =
                    object_shared_quality_values(reference_obj, quality, &state.all_creature_types);
                object_shares_quality_values(obj, quality, &values, &state.all_creature_types)
            })
    })
}

/// CR 608.2b: Validate that all targeted objects share at least one value of the named quality.
/// This is a group constraint that cannot be checked per-object — it requires the full set.
/// Checked at resolution time per CR 608.2b (verifying target legality on resolution).
///
/// Returns `true` if the constraint is satisfied (or if there are fewer than 2 targets).
/// For "creature type": all objects must share at least one creature subtype.
/// For "color": all objects must share at least one color.
/// For "card type": all objects must share at least one card type.
pub fn validate_shares_quality(
    state: &GameState,
    targets: &[TargetRef],
    quality: &SharedQuality,
) -> bool {
    let obj_ids: Vec<ObjectId> = targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();

    // Fewer than 2 objects — constraint is trivially satisfied.
    if obj_ids.len() < 2 {
        return true;
    }

    let mut sets = Vec::new();
    for id in obj_ids {
        let Some(obj) = state.objects.get(&id) else {
            return false;
        };
        sets.push(object_shared_quality_values(
            obj,
            quality,
            &state.all_creature_types,
        ));
    }

    let mut shared = sets[0].clone();
    for set in &sets[1..] {
        shared = shared.intersection(set).cloned().collect();
    }
    !shared.is_empty()
}

/// Check if a player matches a typed player filter.
///
/// Used by static abilities that target players rather than objects.
pub fn player_matches_filter(
    player_id: PlayerId,
    filter: &str,
    source_controller: Option<PlayerId>,
) -> bool {
    for part in filter.split('+') {
        match part {
            "You" if source_controller != Some(player_id) => {
                return false;
            }
            "Opp" if source_controller == Some(player_id) => {
                return false;
            }
            _ => {}
        }
    }
    true
}

// ---------------------------------------------------------------------------
// CR 115.9c: "that targets only [X]" shared helpers
// ---------------------------------------------------------------------------

/// CR 115.9c: Extract the first `TargetsOnly` inner filter from a filter tree.
/// Walks through Or/And/Typed branches to find a `FilterProp::TargetsOnly`.
pub(crate) fn extract_targets_only(filter: &TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(tf) => {
            for prop in &tf.properties {
                if let FilterProp::TargetsOnly { filter } = prop {
                    return Some(*filter.clone());
                }
            }
            None
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            // All branches should have the same TargetsOnly (distributed by parser);
            // return the first one found.
            filters.iter().find_map(extract_targets_only)
        }
        _ => None,
    }
}

/// CR 115.9b: Extract the first `Targets` inner filter from a filter tree.
/// Walks through Or/And/Typed branches to find a `FilterProp::Targets`.
pub(crate) fn extract_targets(filter: &TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(tf) => {
            for prop in &tf.properties {
                if let FilterProp::Targets { filter } = prop {
                    return Some(*filter.clone());
                }
            }
            None
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().find_map(extract_targets)
        }
        _ => None,
    }
}

/// Check if a player target matches a TargetFilter constraint.
/// CR 115.9c: Used to validate player targets in "that targets only [X]" checks.
pub fn player_matches_target_filter(
    filter: &TargetFilter,
    player_id: PlayerId,
    source_controller: Option<PlayerId>,
) -> bool {
    match filter {
        TargetFilter::Any | TargetFilter::Player => true,
        TargetFilter::SelfRef => false, // SelfRef refers to objects, not players
        TargetFilter::Controller => source_controller == Some(player_id),
        TargetFilter::ScopedPlayer => false,
        TargetFilter::Typed(ref tf) if tf.type_filters.is_empty() => match &tf.controller {
            Some(ControllerRef::You) => source_controller == Some(player_id),
            Some(ControllerRef::Opponent) => source_controller.is_some_and(|c| c != player_id),
            Some(ControllerRef::ScopedPlayer) => false,
            // CR 109.4: TargetPlayer has no meaning when matching a player against
            // a filter without ability context. Fail closed (mirrors the pattern
            // established at filter.rs:526–569 for spell-record filters).
            Some(ControllerRef::TargetPlayer) => false,
            Some(ControllerRef::DefendingPlayer) => false,
            None => true,
        },
        // Typed filters with type_filters don't match players
        TargetFilter::Typed(_) => false,
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|f| player_matches_target_filter(f, player_id, source_controller)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|f| player_matches_target_filter(f, player_id, source_controller)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, AggregateFunction, AttachmentKind, ChosenAttribute,
        Comparator, ControllerRef, Effect, FilterProp, ManaContribution, ManaProduction,
        PlayerScope, QuantityExpr, QuantityRef, ReplacementDefinition, StaticDefinition,
        TargetFilter, TriggerDefinition,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::events::GameEvent;
    use crate::types::game_state::{AttachmentSnapshot, ZoneChangeRecord};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    /// Terse 4-arg wrapper for filter-matching tests.
    ///
    /// Builds a bare `FilterContext::from_source` and delegates. Shadows the
    /// public `matches_target_filter` (which takes a `&FilterContext`) so the
    /// existing test bodies remain compact.
    #[allow(clippy::module_name_repetitions)]
    fn matches_target_filter(
        state: &GameState,
        object_id: ObjectId,
        filter: &TargetFilter,
        source_id: ObjectId,
    ) -> bool {
        super::matches_target_filter(
            state,
            object_id,
            filter,
            &FilterContext::from_source(state, source_id),
        )
    }

    /// Explicit-controller variant used by tests that exercise stack-resolving
    /// paths where the source has left play.
    #[allow(dead_code)]
    fn matches_target_filter_controlled(
        state: &GameState,
        object_id: ObjectId,
        filter: &TargetFilter,
        source_id: ObjectId,
        controller: PlayerId,
    ) -> bool {
        super::matches_target_filter(
            state,
            object_id,
            filter,
            &FilterContext::from_source_with_controller(source_id, controller),
        )
    }

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    #[test]
    fn none_filter_matches_nothing() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(!matches_target_filter(&state, id, &TargetFilter::None, id));
    }

    #[test]
    fn any_filter_matches_everything() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(matches_target_filter(&state, id, &TargetFilter::Any, id));
    }

    #[test]
    fn type_filter_matches_correct_type() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let creature_filter = TargetFilter::Typed(TypedFilter::creature());
        let land_filter = TargetFilter::Typed(TypedFilter::land());
        let card_filter = TargetFilter::Typed(TypedFilter::card());
        assert!(matches_target_filter(&state, id, &creature_filter, id));
        assert!(!matches_target_filter(&state, id, &land_filter, id));
        assert!(matches_target_filter(&state, id, &card_filter, id));
    }

    #[test]
    fn self_filter() {
        let mut state = setup();
        let a = add_creature(&mut state, PlayerId(0), "A");
        let b = add_creature(&mut state, PlayerId(0), "B");
        assert!(matches_target_filter(&state, a, &TargetFilter::SelfRef, a));
        assert!(!matches_target_filter(&state, b, &TargetFilter::SelfRef, a));
    }

    #[test]
    fn other_filter_excludes_source() {
        let mut state = setup();
        let marshal = add_creature(&mut state, PlayerId(0), "Benalish Marshal");
        let bear = add_creature(&mut state, PlayerId(0), "Bear");

        // "Creature.Other+YouCtrl" = And(Typed{creature, You}, Not(SelfRef))
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::SelfRef),
                },
            ],
        };

        // Marshal should NOT match its own "Other" filter
        assert!(!matches_target_filter(&state, marshal, &filter, marshal));
        // Bear should match
        assert!(matches_target_filter(&state, bear, &filter, marshal));
    }

    #[test]
    fn you_ctrl_filter() {
        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let filter = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));

        assert!(matches_target_filter(&state, mine, &filter, mine));
        assert!(!matches_target_filter(&state, theirs, &filter, mine));
    }

    #[test]
    fn with_keyword_matches_case_insensitively() {
        let mut state = setup();
        let bird = add_creature(&mut state, PlayerId(0), "Bird");
        state
            .objects
            .get_mut(&bird)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::WithKeyword {
                value: Keyword::Flying,
            },
        ]));
        assert!(matches_target_filter(&state, bird, &filter, bird));
    }

    #[test]
    fn spell_record_matches_qualified_filter() {
        let record = SpellCastRecord {
            core_types: vec![CoreType::Creature],
            supertypes: vec![Supertype::Legendary],
            subtypes: vec!["Bird".to_string()],
            keywords: vec![Keyword::Flying],
            colors: vec![ManaColor::Blue],
            mana_value: 3,
            has_x_in_cost: false,
        };
        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .with_type(TypeFilter::Subtype("Bird".to_string()))
                .properties(vec![
                    FilterProp::WithKeyword {
                        value: Keyword::Flying,
                    },
                    FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Legendary,
                    },
                    FilterProp::HasColor {
                        color: ManaColor::Blue,
                    },
                ]),
        );
        assert!(spell_record_matches_filter(
            &record,
            &filter,
            PlayerId(0),
            &[]
        ));
    }

    /// CR 107.3 + CR 202.1: `FilterProp::HasXInManaCost` reads
    /// `SpellCastRecord::has_x_in_cost` — matches only when the recorded spell's
    /// printed mana cost contained an `{X}` shard. Parallel record without
    /// `has_x_in_cost` must NOT match.
    #[test]
    fn spell_record_has_x_in_cost_filter() {
        let x_record = SpellCastRecord {
            core_types: vec![CoreType::Creature],
            supertypes: vec![],
            subtypes: vec![],
            keywords: vec![],
            colors: vec![],
            mana_value: 3,
            has_x_in_cost: true,
        };
        let non_x_record = SpellCastRecord {
            has_x_in_cost: false,
            ..x_record.clone()
        };
        let filter = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
        );
        assert!(
            spell_record_matches_filter(&x_record, &filter, PlayerId(0), &[]),
            "record with X in cost must match HasXInManaCost filter"
        );
        assert!(
            !spell_record_matches_filter(&non_x_record, &filter, PlayerId(0), &[]),
            "record without X in cost must NOT match HasXInManaCost filter"
        );
    }

    #[test]
    fn object_has_mana_ability_filter_uses_mana_ability_classifier() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let mana_rock = create_object(
            &mut state,
            CardId(410),
            PlayerId(0),
            "Mana Rock".to_string(),
            Zone::Battlefield,
        );
        let draw_rock = create_object(
            &mut state,
            CardId(411),
            PlayerId(0),
            "Draw Rock".to_string(),
            Zone::Battlefield,
        );

        for id in [mana_rock, draw_rock] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Artifact);
        }
        let mana_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        let draw_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        std::sync::Arc::make_mut(&mut state.objects.get_mut(&mana_rock).unwrap().abilities)
            .push(mana_ability);
        std::sync::Arc::make_mut(&mut state.objects.get_mut(&draw_rock).unwrap().abilities)
            .push(draw_ability);

        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::HasManaAbility]),
        );

        assert!(matches_target_filter(&state, mana_rock, &filter, source));
        assert!(!matches_target_filter(&state, draw_rock, &filter, source));
    }

    #[test]
    fn object_has_no_abilities_filter_checks_all_ability_kinds() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let vanilla = add_creature(&mut state, PlayerId(0), "Vanilla");
        let keyworded = add_creature(&mut state, PlayerId(0), "Keyworded");
        let activated = add_creature(&mut state, PlayerId(0), "Activated");
        let triggered = add_creature(&mut state, PlayerId(0), "Triggered");
        let replacement = add_creature(&mut state, PlayerId(0), "Replacement");
        let static_ability = add_creature(&mut state, PlayerId(0), "Static");

        state
            .objects
            .get_mut(&keyworded)
            .unwrap()
            .keywords
            .push(Keyword::Flying);
        std::sync::Arc::make_mut(&mut state.objects.get_mut(&activated).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ),
        );
        state
            .objects
            .get_mut(&triggered)
            .unwrap()
            .trigger_definitions
            .push(TriggerDefinition::new(TriggerMode::ChangesZone));
        state
            .objects
            .get_mut(&replacement)
            .unwrap()
            .replacement_definitions
            .push(ReplacementDefinition::new(ReplacementEvent::ChangeZone));
        state
            .objects
            .get_mut(&static_ability)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::Continuous));

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::HasNoAbilities]),
        );

        assert!(matches_target_filter(&state, vanilla, &filter, source));
        assert!(!matches_target_filter(&state, keyworded, &filter, source));
        assert!(!matches_target_filter(&state, activated, &filter, source));
        assert!(!matches_target_filter(&state, triggered, &filter, source));
        assert!(!matches_target_filter(&state, replacement, &filter, source));
        assert!(!matches_target_filter(
            &state,
            static_ability,
            &filter,
            source
        ));
    }

    #[test]
    fn exact_mana_cost_filter_does_not_match_same_mana_value() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let zero = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Zero Artifact".to_string(),
            Zone::Battlefield,
        );
        let one = create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "One Artifact".to_string(),
            Zone::Battlefield,
        );
        let white = create_object(
            &mut state,
            CardId(402),
            PlayerId(0),
            "White Artifact".to_string(),
            Zone::Battlefield,
        );

        for id in [zero, one, white] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Artifact);
        }
        state.objects.get_mut(&zero).unwrap().mana_cost = ManaCost::zero();
        state.objects.get_mut(&one).unwrap().mana_cost = ManaCost::generic(1);
        state.objects.get_mut(&white).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };

        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
            FilterProp::ManaCostIn {
                costs: vec![ManaCost::zero(), ManaCost::generic(1)],
            },
        ]));

        assert!(matches_target_filter(&state, zero, &filter, source));
        assert!(matches_target_filter(&state, one, &filter, source));
        assert!(!matches_target_filter(&state, white, &filter, source));
    }

    #[test]
    fn opp_ctrl_filter() {
        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));

        assert!(!matches_target_filter(&state, mine, &filter, mine));
        assert!(matches_target_filter(&state, theirs, &filter, mine));
    }

    #[test]
    fn combined_type_and_controller() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Lord");
        let ally = add_creature(&mut state, PlayerId(0), "Ally");
        let enemy = add_creature(&mut state, PlayerId(1), "Enemy");

        // "Creature.Other+YouCtrl"
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::SelfRef),
                },
            ],
        };

        assert!(!matches_target_filter(&state, source, &filter, source));
        assert!(matches_target_filter(&state, ally, &filter, source));
        assert!(!matches_target_filter(&state, enemy, &filter, source));
    }

    #[test]
    fn permanent_matches_multiple_types() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let filter = TargetFilter::Typed(TypedFilter::permanent());
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn enchanted_by_only_matches_attached_creature() {
        let mut state = setup();
        let creature_a = add_creature(&mut state, PlayerId(0), "Bear A");
        let creature_b = add_creature(&mut state, PlayerId(0), "Bear B");

        // Create an aura (source) attached to creature_a
        let next_id = state.next_object_id;
        let aura = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(0),
            "Rancor".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        state.objects.get_mut(&aura).unwrap().attached_to = Some(creature_a.into());

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));

        assert!(matches_target_filter(&state, creature_a, &filter, aura));
        assert!(
            !matches_target_filter(&state, creature_b, &filter, aura),
            "EnchantedBy must not match creatures the aura is NOT attached to"
        );
    }

    #[test]
    fn attached_to_source_matches_aura_or_equipment_attached_to_source() {
        // CR 301.5 + CR 303.4: `FilterProp::AttachedToSource` matches when the
        // candidate object's `attached_to` references the filter source.
        // Inverse of `EnchantedBy`/`EquippedBy`. Drives Kellan, the Fae-Blooded's
        // "for each Aura and Equipment attached to ~" boost multiplier.
        let mut state = setup();
        let kellan = add_creature(&mut state, PlayerId(0), "Kellan");
        let other_creature = add_creature(&mut state, PlayerId(0), "Other");

        let aura_id = state.next_object_id;
        let aura = create_object(
            &mut state,
            CardId(aura_id),
            PlayerId(0),
            "Rancor".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        state.objects.get_mut(&aura).unwrap().attached_to = Some(kellan.into());

        let equip_id = state.next_object_id;
        let equip = create_object(
            &mut state,
            CardId(equip_id),
            PlayerId(0),
            "Sword".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equip)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state.objects.get_mut(&equip).unwrap().attached_to = Some(other_creature.into());

        let filter = TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::AttachedToSource]),
        );

        assert!(
            matches_target_filter(&state, aura, &filter, kellan),
            "AttachedToSource must match an attachment on the source"
        );
        assert!(
            !matches_target_filter(&state, equip, &filter, kellan),
            "AttachedToSource must NOT match an attachment on a different object"
        );
        assert!(
            !matches_target_filter(&state, kellan, &filter, kellan),
            "AttachedToSource must NOT match the source itself (it is not attached)"
        );
    }

    #[test]
    fn attached_to_recipient_matches_attachments_on_layer_recipient() {
        // CR 301.5 + CR 303.4 + CR 613.4c: `FilterProp::AttachedToRecipient`
        // matches when the candidate object's `attached_to` references the
        // *recipient* of the resolving continuous modification — used by
        // Aura/Equipment statics whose Oracle text says "for each X attached
        // to it" (Strong Back, Bruenor Battlehammer, Mantle of the Ancients).
        // Crucially, the predicate is FALSE when the matching is performed
        // against attachments on the source rather than the recipient: that's
        // exactly the bug that produced flat +0/+0 boosts for Strong Back.
        let mut state = setup();
        let strong_back = add_creature(&mut state, PlayerId(0), "Strong Back"); // playing source role
        let enchanted_creature = add_creature(&mut state, PlayerId(0), "Equipped Bear");
        let unrelated_creature = add_creature(&mut state, PlayerId(0), "Other Bear");

        // Two attachments on the enchanted creature — the recipient.
        let aura_id = state.next_object_id;
        let aura = create_object(
            &mut state,
            CardId(aura_id),
            PlayerId(0),
            "Rancor".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        state.objects.get_mut(&aura).unwrap().attached_to = Some(enchanted_creature.into());

        let equip_id = state.next_object_id;
        let equip = create_object(
            &mut state,
            CardId(equip_id),
            PlayerId(0),
            "Sword".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equip)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state.objects.get_mut(&equip).unwrap().attached_to = Some(enchanted_creature.into());

        // One unrelated attachment — on a different creature, must not count.
        let bystander_id = state.next_object_id;
        let bystander = create_object(
            &mut state,
            CardId(bystander_id),
            PlayerId(0),
            "Wild Growth".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bystander)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        state.objects.get_mut(&bystander).unwrap().attached_to = Some(unrelated_creature.into());

        let filter = TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::AttachedToRecipient]),
        );

        // Recipient bound to enchanted_creature: aura and equip match,
        // bystander does not.
        let ctx =
            FilterContext::from_source_with_recipient(&state, strong_back, enchanted_creature);
        assert!(
            super::matches_target_filter(&state, aura, &filter, &ctx),
            "AttachedToRecipient must match an attachment on the recipient"
        );
        assert!(
            super::matches_target_filter(&state, equip, &filter, &ctx),
            "AttachedToRecipient must match every attachment on the recipient"
        );
        assert!(
            !super::matches_target_filter(&state, bystander, &filter, &ctx),
            "AttachedToRecipient must NOT match attachments on a different creature"
        );

        // CR 109.3: When no recipient is bound (e.g., trigger-time
        // resolution where "it" refers to the trigger's source — Catti-brie,
        // Wyleth), AttachedToRecipient falls back to source-attachment
        // semantics. With strong_back as the source, attachments-on-source
        // is empty, so neither aura nor equip match.
        let ctx_source_only = FilterContext::from_source(&state, strong_back);
        assert!(
            !super::matches_target_filter(&state, aura, &filter, &ctx_source_only),
            "Without recipient, must check attachments on source — strong_back has none"
        );

        // But with the source itself = the bear, attachments-on-source IS
        // the right answer — confirms the trigger-self-source case.
        let ctx_source_is_recipient = FilterContext::from_source(&state, enchanted_creature);
        assert!(
            super::matches_target_filter(&state, aura, &filter, &ctx_source_is_recipient),
            "When source = the affected creature (trigger-self pattern), \
             AttachedToRecipient must match attachments on the source"
        );
    }

    #[test]
    fn enchanted_by_no_attachment_matches_nothing() {
        let mut state = setup();
        let creature = add_creature(&mut state, PlayerId(0), "Bear");

        // Aura not attached to anything
        let next_id = state.next_object_id;
        let aura = create_object(
            &mut state,
            CardId(next_id),
            PlayerId(0),
            "Floating Aura".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));

        assert!(
            !matches_target_filter(&state, creature, &filter, aura),
            "Unattached aura should not match any creature"
        );
    }

    #[test]
    fn player_filter_you() {
        assert!(player_matches_filter(PlayerId(0), "You", Some(PlayerId(0))));
        assert!(!player_matches_filter(
            PlayerId(1),
            "You",
            Some(PlayerId(0))
        ));
    }

    #[test]
    fn player_filter_opp() {
        assert!(!player_matches_filter(
            PlayerId(0),
            "Opp",
            Some(PlayerId(0))
        ));
        assert!(player_matches_filter(PlayerId(1), "Opp", Some(PlayerId(0))));
    }

    #[test]
    fn not_filter_inverts() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let not_self = TargetFilter::Not {
            filter: Box::new(TargetFilter::SelfRef),
        };
        assert!(!matches_target_filter(&state, id, &not_self, id));
    }

    #[test]
    fn or_filter_any_match() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::land()),
                TargetFilter::Typed(TypedFilter::creature()),
            ],
        };
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn tapped_property() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        state.objects.get_mut(&id).unwrap().tapped = true;

        let filter =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Tapped]));
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn has_supertype_basic_matches_basic_land() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Plains");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Basic);
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];

        let filter =
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Basic,
                }]),
            );
        assert!(matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn has_supertype_basic_rejects_nonbasic_land() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Stomping Ground");
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];

        let filter =
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Basic,
                }]),
            );
        assert!(!matches_target_filter(&state, id, &filter, id));
    }

    #[test]
    fn controlled_variant_uses_explicit_controller() {
        let mut state = setup();
        let obj = add_creature(&mut state, PlayerId(1), "Theirs");

        let filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));

        // Source doesn't exist, but we pass controller explicitly
        let fake_source = ObjectId(9999);
        assert!(matches_target_filter_controlled(
            &state,
            obj,
            &filter,
            fake_source,
            PlayerId(0)
        ));
    }

    #[test]
    fn chosen_creature_type_matches_subtype() {
        use crate::types::ability::ChosenAttribute;

        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Mimic");
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::CreatureType("Elf".to_string()));

        let elf = add_creature(&mut state, PlayerId(0), "Elf Warrior");
        state
            .objects
            .get_mut(&elf)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".to_string());

        let goblin = add_creature(&mut state, PlayerId(0), "Goblin");
        state
            .objects
            .get_mut(&goblin)
            .unwrap()
            .card_types
            .subtypes
            .push("Goblin".to_string());

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::IsChosenCreatureType]),
        );

        assert!(
            matches_target_filter(&state, elf, &filter, source),
            "Elf should match chosen creature type Elf"
        );
        assert!(
            !matches_target_filter(&state, goblin, &filter, source),
            "Goblin should not match chosen creature type Elf"
        );
    }

    #[test]
    fn attacking_property_matches_only_declared_attackers() {
        use crate::game::combat::{AttackerInfo, CombatState};

        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        let bystander = add_creature(&mut state, PlayerId(0), "Bystander");
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            ..CombatState::default()
        });

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Attacking]));

        assert!(matches_target_filter(&state, attacker, &filter, attacker));
        assert!(!matches_target_filter(&state, bystander, &filter, attacker));
    }

    #[test]
    fn blocking_source_property_matches_only_source_blockers() {
        use crate::game::combat::{AttackerInfo, CombatState};

        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        let other_attacker = add_creature(&mut state, PlayerId(0), "Other Attacker");
        let blocker = add_creature(&mut state, PlayerId(1), "Blocker");
        let other_blocker = add_creature(&mut state, PlayerId(1), "Other Blocker");
        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(attacker, PlayerId(1)),
                AttackerInfo::attacking_player(other_attacker, PlayerId(1)),
            ],
            blocker_assignments: [
                (attacker, vec![blocker]),
                (other_attacker, vec![other_blocker]),
            ]
            .into(),
            blocker_to_attacker: [
                (blocker, vec![attacker]),
                (other_blocker, vec![other_attacker]),
            ]
            .into(),
            ..CombatState::default()
        });

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::BlockingSource]),
        );

        assert!(matches_target_filter(&state, blocker, &filter, attacker));
        assert!(!matches_target_filter(
            &state,
            other_blocker,
            &filter,
            attacker,
        ));
    }

    #[test]
    fn exiled_by_source_matches_linked_objects() {
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let exiled = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled Card".into(),
            Zone::Exile,
        );
        let unlinked = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Other Card".into(),
            Zone::Exile,
        );

        // CR 610.3: ExileLink records which objects were exiled by which source.
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });

        let filter = TargetFilter::ExiledBySource;
        assert!(matches_target_filter(&state, exiled, &filter, source));
        assert!(
            !matches_target_filter(&state, unlinked, &filter, source),
            "unlinked object should not match ExiledBySource"
        );
    }

    #[test]
    fn shares_quality_creature_type_passes_with_shared_subtype() {
        let mut state = setup();
        state.all_creature_types = vec!["Elf".to_string()];
        let a = add_creature(&mut state, PlayerId(0), "Elf Warrior");
        state
            .objects
            .get_mut(&a)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".to_string());

        let b = add_creature(&mut state, PlayerId(0), "Elf Druid");
        state
            .objects
            .get_mut(&b)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".to_string());

        let targets = vec![TargetRef::Object(a), TargetRef::Object(b)];
        assert!(
            validate_shares_quality(&state, &targets, &SharedQuality::CreatureType),
            "Two Elves should share the Elf creature type"
        );
    }

    #[test]
    fn shares_quality_creature_type_fails_with_no_shared_subtype() {
        let mut state = setup();
        state.all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];
        let a = add_creature(&mut state, PlayerId(0), "Elf");
        state
            .objects
            .get_mut(&a)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".to_string());

        let b = add_creature(&mut state, PlayerId(0), "Goblin");
        state
            .objects
            .get_mut(&b)
            .unwrap()
            .card_types
            .subtypes
            .push("Goblin".to_string());

        let targets = vec![TargetRef::Object(a), TargetRef::Object(b)];
        assert!(
            !validate_shares_quality(&state, &targets, &SharedQuality::CreatureType),
            "Elf and Goblin share no creature types"
        );
    }

    #[test]
    fn shares_quality_color_passes_with_shared_color() {
        let mut state = setup();
        let a = add_creature(&mut state, PlayerId(0), "Blue Red A");
        state.objects.get_mut(&a).unwrap().color = vec![ManaColor::Blue, ManaColor::Red];

        let b = add_creature(&mut state, PlayerId(0), "Blue Green B");
        state.objects.get_mut(&b).unwrap().color = vec![ManaColor::Blue, ManaColor::Green];

        let targets = vec![TargetRef::Object(a), TargetRef::Object(b)];
        assert!(
            validate_shares_quality(&state, &targets, &SharedQuality::Color),
            "Both share Blue"
        );
    }

    #[test]
    fn shares_quality_color_fails_with_no_shared_color() {
        let mut state = setup();
        let a = add_creature(&mut state, PlayerId(0), "Red A");
        state.objects.get_mut(&a).unwrap().color = vec![ManaColor::Red];

        let b = add_creature(&mut state, PlayerId(0), "Blue B");
        state.objects.get_mut(&b).unwrap().color = vec![ManaColor::Blue];

        let targets = vec![TargetRef::Object(a), TargetRef::Object(b)];
        assert!(
            !validate_shares_quality(&state, &targets, &SharedQuality::Color),
            "Red and Blue share no colors"
        );
    }

    #[test]
    fn shares_quality_with_source_color_matches_per_object() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Blue Source");
        state.objects.get_mut(&source).unwrap().color = vec![ManaColor::Blue];
        let blue = add_creature(&mut state, PlayerId(0), "Blue Candidate");
        state.objects.get_mut(&blue).unwrap().color = vec![ManaColor::Blue];
        let red = add_creature(&mut state, PlayerId(0), "Red Candidate");
        state.objects.get_mut(&red).unwrap().color = vec![ManaColor::Red];

        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::SharesQuality {
                quality: SharedQuality::Color,
                reference: Some(Box::new(TargetFilter::SelfRef)),
                relation: SharedQualityRelation::Shares,
            },
        ]));

        assert!(matches_target_filter(&state, blue, &filter, source));
        assert!(!matches_target_filter(&state, red, &filter, source));
    }

    #[test]
    fn shares_quality_reference_can_use_discarded_trigger_object() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Diviner");
        let discarded = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Discarded Instant".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&discarded)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let instant = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Candidate Instant".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let sorcery = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Candidate Sorcery".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&sorcery)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        state.current_trigger_event = Some(GameEvent::Discarded {
            player_id: PlayerId(0),
            object_id: discarded,
        });

        let filter =
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::CardType,
                    reference: Some(Box::new(TargetFilter::TriggeringSource)),
                    relation: SharedQualityRelation::Shares,
                }]),
            );

        assert!(matches_target_filter(&state, instant, &filter, source));
        assert!(!matches_target_filter(&state, sorcery, &filter, source));
    }

    #[test]
    fn shares_quality_reference_can_use_second_batched_discard_event_object() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Diviner");
        let discarded_creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Discarded Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&discarded_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let discarded_instant = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Discarded Instant".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&discarded_instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let instant = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Candidate Instant".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let sorcery = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Candidate Sorcery".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&sorcery)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        state.current_trigger_event = Some(GameEvent::Discarded {
            player_id: PlayerId(0),
            object_id: discarded_creature,
        });
        state.current_trigger_events = vec![
            GameEvent::Discarded {
                player_id: PlayerId(0),
                object_id: discarded_creature,
            },
            GameEvent::Discarded {
                player_id: PlayerId(0),
                object_id: discarded_instant,
            },
        ];

        let filter =
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::CardType,
                    reference: Some(Box::new(TargetFilter::TriggeringSource)),
                    relation: SharedQualityRelation::Shares,
                }]),
            );

        assert!(
            matches_target_filter(&state, instant, &filter, source),
            "candidate should match the second discarded card's Instant type"
        );
        assert!(!matches_target_filter(&state, sorcery, &filter, source));
    }

    #[test]
    fn shares_quality_negated_land_type_reference_matches_per_object() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let plains = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&plains).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Plains".to_string());
        }
        let island = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&island).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
        }
        let mountain = create_object(
            &mut state,
            CardId(102),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mountain).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
        }

        let filter =
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::LandType,
                    reference: Some(Box::new(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    ))),
                    relation: SharedQualityRelation::DoesNotShare,
                }]),
            );

        assert!(!matches_target_filter(&state, plains, &filter, source));
        assert!(!matches_target_filter(&state, island, &filter, source));
        assert!(matches_target_filter(&state, mountain, &filter, source));
    }

    #[test]
    fn shares_quality_name_reference_matches_graveyard_card() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let reference = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Frost Bolt".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&reference)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let matching = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Frost Bolt".to_string(),
            Zone::Library,
        );
        let other = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Fire Bolt".to_string(),
            Zone::Library,
        );

        let filter = TargetFilter::Typed(TypedFilter::default().properties(vec![
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(Box::new(TargetFilter::Typed(
                    TypedFilter::default()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone {
                            zone: Zone::Graveyard,
                        }]),
                ))),
                relation: SharedQualityRelation::Shares,
            },
        ]));

        assert!(matches_target_filter(&state, matching, &filter, source));
        assert!(!matches_target_filter(&state, other, &filter, source));
    }

    #[test]
    fn shares_quality_name_negated_reference_uses_explicit_battlefield_zone() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let battlefield_room = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Central Elevator".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&battlefield_room)
            .unwrap()
            .card_types
            .subtypes
            .push("Room".to_string());
        let library_room_same_name = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Hidden Elevator".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&library_room_same_name)
            .unwrap()
            .card_types
            .subtypes
            .push("Room".to_string());

        let matching = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Central Elevator".to_string(),
            Zone::Library,
        );
        let different = create_object(
            &mut state,
            CardId(103),
            PlayerId(0),
            "Promising Stairs".to_string(),
            Zone::Library,
        );

        let room_reference = TargetFilter::Typed(
            TypedFilter::default()
                .controller(ControllerRef::You)
                .subtype("Room".to_string())
                .properties(vec![FilterProp::InZone {
                    zone: Zone::Battlefield,
                }]),
        );
        let filter = TargetFilter::Typed(TypedFilter::default().properties(vec![
            FilterProp::SharesQuality {
                quality: SharedQuality::Name,
                reference: Some(Box::new(room_reference)),
                relation: SharedQualityRelation::DoesNotShare,
            },
        ]));

        assert!(!matches_target_filter(&state, matching, &filter, source));
        assert!(matches_target_filter(&state, different, &filter, source));
    }

    #[test]
    fn attacked_this_turn_matches_tracked_creature() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        let bystander = add_creature(&mut state, PlayerId(0), "Bystander");
        state.creatures_attacked_this_turn.insert(attacker);

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::AttackedThisTurn]),
        );

        assert!(matches_target_filter(&state, attacker, &filter, attacker));
        assert!(!matches_target_filter(&state, bystander, &filter, attacker));
    }

    #[test]
    fn attacked_this_turn_works_post_combat() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        state.creatures_attacked_this_turn.insert(attacker);
        // combat is None post-combat — filter should still match via HashSet
        assert!(state.combat.is_none());

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::AttackedThisTurn]),
        );
        assert!(matches_target_filter(&state, attacker, &filter, attacker));
    }

    #[test]
    fn blocked_this_turn_matches_tracked_creature() {
        let mut state = setup();
        let blocker = add_creature(&mut state, PlayerId(1), "Blocker");
        let bystander = add_creature(&mut state, PlayerId(1), "Bystander");
        state.creatures_blocked_this_turn.insert(blocker);

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::BlockedThisTurn]),
        );

        assert!(matches_target_filter(&state, blocker, &filter, blocker));
        assert!(!matches_target_filter(&state, bystander, &filter, blocker));
    }

    #[test]
    fn attacked_or_blocked_this_turn_matches_either() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        let blocker = add_creature(&mut state, PlayerId(1), "Blocker");
        let neither = add_creature(&mut state, PlayerId(0), "Bystander");
        state.creatures_attacked_this_turn.insert(attacker);
        state.creatures_blocked_this_turn.insert(blocker);

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::AttackedOrBlockedThisTurn]),
        );

        assert!(matches_target_filter(&state, attacker, &filter, attacker));
        assert!(matches_target_filter(&state, blocker, &filter, attacker));
        assert!(!matches_target_filter(&state, neither, &filter, attacker));
    }

    #[test]
    fn normalize_contextual_filter_without_parent_targets_rewrites_not_parent_to_any() {
        let filter = TargetFilter::Not {
            filter: Box::new(TargetFilter::ParentTarget),
        };

        assert_eq!(normalize_contextual_filter(&filter, &[]), TargetFilter::Any);
    }

    #[test]
    fn normalize_contextual_filter_with_parent_target_excludes_specific_object() {
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature()),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::ParentTarget),
                },
            ],
        };

        let normalized = normalize_contextual_filter(&filter, &[TargetRef::Object(ObjectId(7))]);
        assert_eq!(
            normalized,
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::SpecificObject { id: ObjectId(7) }),
                    },
                ],
            }
        );
    }

    #[test]
    fn normalize_contextual_filter_with_multiple_parent_targets_excludes_all_of_them() {
        let filter = TargetFilter::Not {
            filter: Box::new(TargetFilter::ParentTarget),
        };

        assert_eq!(
            normalize_contextual_filter(
                &filter,
                &[
                    TargetRef::Object(ObjectId(7)),
                    TargetRef::Object(ObjectId(8))
                ]
            ),
            TargetFilter::Not {
                filter: Box::new(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::SpecificObject { id: ObjectId(7) },
                        TargetFilter::SpecificObject { id: ObjectId(8) },
                    ],
                }),
            }
        );
    }

    #[test]
    fn has_chosen_name_matches_object_with_chosen_card_name() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Sorcerer");
        let bolt = add_creature(&mut state, PlayerId(0), "Lightning Bolt");
        let growth = add_creature(&mut state, PlayerId(0), "Giant Growth");

        // Set chosen name on source
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::CardName("Lightning Bolt".to_string()));

        assert!(matches_target_filter(
            &state,
            bolt,
            &TargetFilter::HasChosenName,
            source,
        ));
        assert!(!matches_target_filter(
            &state,
            growth,
            &TargetFilter::HasChosenName,
            source,
        ));
    }

    #[test]
    fn has_chosen_name_returns_false_when_no_card_name_chosen() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Sorcerer");
        let bolt = add_creature(&mut state, PlayerId(0), "Lightning Bolt");

        // Source has no chosen attributes
        assert!(!matches_target_filter(
            &state,
            bolt,
            &TargetFilter::HasChosenName,
            source,
        ));
    }

    #[test]
    fn named_filter_matches_by_literal_name() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Sorcerer");
        let bolt = add_creature(&mut state, PlayerId(0), "Lightning Bolt");
        let growth = add_creature(&mut state, PlayerId(0), "Giant Growth");

        let filter = TargetFilter::Named {
            name: "Lightning Bolt".to_string(),
        };
        assert!(matches_target_filter(&state, bolt, &filter, source));
        assert!(!matches_target_filter(&state, growth, &filter, source));
    }

    #[test]
    fn spell_object_filter_uses_caster_and_zone() {
        let mut state = setup();
        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(1),
            "Borrowed Spell".to_string(),
            Zone::Exile,
        );
        let spell = state.objects.get_mut(&spell_id).unwrap();
        spell.card_types.core_types.push(CoreType::Sorcery);

        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Sorcery)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone { zone: Zone::Exile }]),
        );

        assert!(spell_object_matches_filter(
            spell,
            PlayerId(0),
            &filter,
            PlayerId(0),
            &[],
        ));
        assert!(!spell_object_matches_filter(
            spell,
            PlayerId(1),
            &filter,
            PlayerId(0),
            &[],
        ));
    }

    #[test]
    fn spell_object_filter_state_resolves_dynamic_cmc_threshold() {
        let mut state = setup();
        state.players[1].life_lost_this_turn = 3;

        let source_id = add_creature(&mut state, PlayerId(0), "Abaddon");
        let small_id = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Small Spell".to_string(),
            Zone::Hand,
        );
        let large_id = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Large Spell".to_string(),
            Zone::Hand,
        );
        let exile_id = create_object(
            &mut state,
            CardId(303),
            PlayerId(0),
            "Exiled Spell".to_string(),
            Zone::Exile,
        );

        for (id, mana_value) in [(small_id, 3), (large_id, 4), (exile_id, 3)] {
            let spell = state.objects.get_mut(&id).unwrap();
            spell.card_types.core_types.push(CoreType::Sorcery);
            spell.mana_cost = ManaCost::generic(mana_value);
        }

        let filter = TargetFilter::Typed(
            TypedFilter::card()
                .controller(ControllerRef::You)
                .properties(vec![
                    FilterProp::InZone { zone: Zone::Hand },
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::LifeLostThisTurn {
                                player: PlayerScope::Opponent {
                                    aggregate: AggregateFunction::Sum,
                                },
                            },
                        },
                    },
                ]),
        );

        let small = state.objects.get(&small_id).unwrap();
        assert!(spell_object_matches_filter_from_state(
            &state,
            small,
            Zone::Hand,
            PlayerId(0),
            &filter,
            source_id,
            &[],
        ));

        let large = state.objects.get(&large_id).unwrap();
        assert!(!spell_object_matches_filter_from_state(
            &state,
            large,
            Zone::Hand,
            PlayerId(0),
            &filter,
            source_id,
            &[],
        ));

        let exiled = state.objects.get(&exile_id).unwrap();
        assert!(!spell_object_matches_filter_from_state(
            &state,
            exiled,
            Zone::Exile,
            PlayerId(0),
            &filter,
            source_id,
            &[],
        ));
    }

    fn add_battlefield_creature_with_cmc(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        cmc: u32,
    ) -> ObjectId {
        use crate::types::mana::ManaCost;
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::generic(cmc);
        id
    }

    /// CR 107.3a + CR 601.2b: `CmcLE { Variable("X") }` with `chosen_x = Some(4)`
    /// matches only objects with CMC ≤ 4.
    #[test]
    fn filter_context_from_ability_resolves_x_in_cmc_le() {
        use crate::types::ability::{
            Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TypedFilter,
        };
        let mut state = setup();
        let cmc2 = add_battlefield_creature_with_cmc(&mut state, PlayerId(0), "Small", 2);
        let cmc4 = add_battlefield_creature_with_cmc(&mut state, PlayerId(0), "Mid", 4);
        let cmc5 = add_battlefield_creature_with_cmc(&mut state, PlayerId(0), "Big", 5);
        let cmc8 = add_battlefield_creature_with_cmc(&mut state, PlayerId(0), "Huge", 8);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: crate::types::ability::Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.chosen_x = Some(4);
        let ctx = FilterContext::from_ability(&ability);

        assert!(super::matches_target_filter(&state, cmc2, &filter, &ctx));
        assert!(super::matches_target_filter(&state, cmc4, &filter, &ctx));
        assert!(!super::matches_target_filter(&state, cmc5, &filter, &ctx));
        assert!(!super::matches_target_filter(&state, cmc8, &filter, &ctx));
    }

    /// CR 208.1 + CR 107.3a: `PowerLE { Variable("X") }` + `chosen_x = Some(3)`
    /// matches only power-≤-3 creatures.
    #[test]
    fn filter_context_from_ability_resolves_x_in_power_le() {
        use crate::types::ability::{
            Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TypedFilter,
        };
        let mut state = setup();
        let weak = add_creature(&mut state, PlayerId(0), "Weak");
        state.objects.get_mut(&weak).unwrap().power = Some(2);
        let strong = add_creature(&mut state, PlayerId(0), "Strong");
        state.objects.get_mut(&strong).unwrap().power = Some(5);

        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::PowerLE {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                }]),
            );
        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);
        let ctx = FilterContext::from_ability(&ability);

        assert!(super::matches_target_filter(&state, weak, &filter, &ctx));
        assert!(!super::matches_target_filter(&state, strong, &filter, &ctx));
    }

    /// CR 107.2: Bare context (no ability in scope) — `Variable("X")` resolves to 0,
    /// so `CmcLE { Variable("X") }` matches nothing with non-zero CMC.
    #[test]
    fn filter_context_bare_resolves_x_to_zero_per_cr_107_2() {
        use crate::types::ability::{QuantityExpr, QuantityRef, TargetFilter, TypedFilter};
        let mut state = setup();
        let cmc2 = add_battlefield_creature_with_cmc(&mut state, PlayerId(0), "Small", 2);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: crate::types::ability::Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let ctx = FilterContext::from_source_with_controller(ObjectId(999), PlayerId(0));
        assert!(!super::matches_target_filter(&state, cmc2, &filter, &ctx));
    }

    /// CR 122.1: `CountersGE { count: Variable("X") }` + `chosen_x = Some(2)` matches
    /// only objects with ≥2 counters of the tracked type.
    #[test]
    fn filter_context_from_ability_resolves_x_in_counters_ge() {
        use crate::types::ability::{
            Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TypedFilter,
        };
        use crate::types::counter::CounterType;
        let mut state = setup();
        let three = add_creature(&mut state, PlayerId(0), "Three");
        state
            .objects
            .get_mut(&three)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);
        let one = add_creature(&mut state, PlayerId(0), "One");
        state
            .objects
            .get_mut(&one)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::CountersGE {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                }]),
            );
        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.chosen_x = Some(2);
        let ctx = FilterContext::from_ability(&ability);

        assert!(super::matches_target_filter(&state, three, &filter, &ctx));
        assert!(!super::matches_target_filter(&state, one, &filter, &ctx));
    }

    /// Serde round-trip for widened `FilterProp::PowerLE.value: QuantityExpr`,
    /// `CountersGE.count: QuantityExpr`, and `Effect::SearchLibrary.count: QuantityExpr`.
    #[test]
    fn widened_numeric_fields_roundtrip_through_json() {
        use crate::types::ability::{Effect, QuantityExpr, TargetFilter, TypedFilter};
        use crate::types::counter::CounterType;

        let power_filter = FilterProp::PowerLE {
            value: QuantityExpr::Fixed { value: 3 },
        };
        let json = serde_json::to_string(&power_filter).unwrap();
        let restored: FilterProp = serde_json::from_str(&json).unwrap();
        assert_eq!(power_filter, restored);

        let counters_filter = FilterProp::CountersGE {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 2 },
        };
        let json = serde_json::to_string(&counters_filter).unwrap();
        let restored: FilterProp = serde_json::from_str(&json).unwrap();
        assert_eq!(counters_filter, restored);

        let search = Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 2 },
            reveal: true,
            target_player: None,
            selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
        };
        let json = serde_json::to_string(&search).unwrap();
        let restored: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(search, restored);
    }

    // CR 303.4: `FilterProp::HasAttachment { Aura, Some(You) }` matches only
    // creatures with at least one Aura whose controller matches the source
    // controller. Killian's "creatures that are enchanted by an Aura you control".
    #[test]
    fn has_attachment_aura_you_matches_only_creatures_with_your_aura() {
        use crate::types::ability::{AttachmentKind, TypeFilter, TypedFilter};
        let mut state = GameState::new_two_player(42);

        // Source (Killian) — controlled by P0.
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Killian".into(),
            Zone::Battlefield,
        );

        // Creature A: has an Aura controlled by P0 → should match.
        let cre_a = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura_a = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Your Aura".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&aura_a).unwrap();
            a.card_types.core_types.push(CoreType::Enchantment);
            a.card_types.subtypes.push("Aura".into());
            a.attached_to = Some(cre_a.into());
        }
        state
            .objects
            .get_mut(&cre_a)
            .unwrap()
            .attachments
            .push(aura_a);

        // Creature B: has an Aura controlled by P1 → should NOT match.
        let cre_b = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Ox".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura_b = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Their Aura".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&aura_b).unwrap();
            a.card_types.core_types.push(CoreType::Enchantment);
            a.card_types.subtypes.push("Aura".into());
            a.attached_to = Some(cre_b.into());
        }
        state
            .objects
            .get_mut(&cre_b)
            .unwrap()
            .attachments
            .push(aura_b);

        // Creature C: no Aura → should NOT match.
        let cre_c = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Wolf".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_c)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(vec![
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
            },
        ]));
        assert!(
            matches_target_filter(&state, cre_a, &filter, source),
            "creature with your aura should match"
        );
        assert!(
            !matches_target_filter(&state, cre_b, &filter, source),
            "creature with opponent's aura should NOT match"
        );
        assert!(
            !matches_target_filter(&state, cre_c, &filter, source),
            "creature without any aura should NOT match"
        );
    }

    // CR 303.4 + CR 301.5: `FilterProp::HasAnyAttachmentOf { [Aura, Equipment] }`
    // matches creatures with at least one Aura OR Equipment attached. Compound-
    // subject grant class (Reyav, Master Smith; Dogmeat, Ever Loyal).
    #[test]
    fn has_any_attachment_of_aura_or_equipment_matches_either() {
        use crate::types::ability::{AttachmentKind, TypeFilter, TypedFilter};
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Reyav".into(),
            Zone::Battlefield,
        );

        // Creature A: enchanted (has an Aura) → should match.
        let cre_a = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "An Aura".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&aura).unwrap();
            a.card_types.core_types.push(CoreType::Enchantment);
            a.card_types.subtypes.push("Aura".into());
            a.attached_to = Some(cre_a.into());
        }
        state
            .objects
            .get_mut(&cre_a)
            .unwrap()
            .attachments
            .push(aura);

        // Creature B: equipped (has an Equipment) → should match.
        let cre_b = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Ox".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let equip = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "An Equipment".into(),
            Zone::Battlefield,
        );
        {
            let e = state.objects.get_mut(&equip).unwrap();
            e.card_types.core_types.push(CoreType::Artifact);
            e.card_types.subtypes.push("Equipment".into());
            e.attached_to = Some(cre_b.into());
        }
        state
            .objects
            .get_mut(&cre_b)
            .unwrap()
            .attachments
            .push(equip);

        // Creature C: no attachments → should NOT match.
        let cre_c = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Wolf".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_c)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(vec![
            FilterProp::HasAnyAttachmentOf {
                kinds: vec![AttachmentKind::Aura, AttachmentKind::Equipment],
                controller: None,
            },
        ]));
        assert!(
            matches_target_filter(&state, cre_a, &filter, source),
            "enchanted creature should match"
        );
        assert!(
            matches_target_filter(&state, cre_b, &filter, source),
            "equipped creature should match"
        );
        assert!(
            !matches_target_filter(&state, cre_c, &filter, source),
            "creature with no attachments should NOT match"
        );
    }

    // CR 303.4: `FilterProp::EnchantedBy` degrades to "has any Aura attached"
    // when the source is not itself an Aura (Hateful Eidolon).
    #[test]
    fn enchanted_by_on_non_aura_source_matches_any_enchanted_creature() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);

        // Source is a non-Aura creature (Hateful Eidolon — attached_to = None).
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Hateful Eidolon".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Enchanted creature.
        let cre_enchanted = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Enchanted".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_enchanted)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "Any Aura".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&aura).unwrap();
            a.card_types.core_types.push(CoreType::Enchantment);
            a.card_types.subtypes.push("Aura".into());
            a.attached_to = Some(cre_enchanted.into());
        }
        state
            .objects
            .get_mut(&cre_enchanted)
            .unwrap()
            .attachments
            .push(aura);

        // Non-enchanted creature.
        let cre_plain = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Plain".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_plain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
        assert!(
            matches_target_filter(&state, cre_enchanted, &filter, source),
            "enchanted creature should match on non-Aura source"
        );
        assert!(
            !matches_target_filter(&state, cre_plain, &filter, source),
            "non-enchanted creature should not match"
        );
    }

    // CR 700.9: A permanent is modified if it has one or more counters on it
    // (CR 122), is equipped (CR 301.5), or is enchanted by an Aura controlled
    // by its controller (CR 303.4).
    #[test]
    fn modified_matches_creature_with_counter() {
        use crate::types::ability::TypedFilter;
        use crate::types::counter::CounterType;
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );

        let cre = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&cre).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.counters.insert(CounterType::Plus1Plus1, 1);
        }

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Modified]));
        assert!(matches_target_filter(&state, cre, &filter, source));
    }

    // CR 301.5: Equipped creatures are modified regardless of Equipment controller.
    #[test]
    fn modified_matches_creature_with_equipment_any_controller() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );

        // Creature controlled by P0, Equipment controlled by P1 — still modified.
        let cre = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let eq = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "Opp Sword".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&eq).unwrap();
            a.card_types.core_types.push(CoreType::Artifact);
            a.card_types.subtypes.push("Equipment".into());
            a.attached_to = Some(cre.into());
        }
        state.objects.get_mut(&cre).unwrap().attachments.push(eq);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Modified]));
        assert!(matches_target_filter(&state, cre, &filter, source));
    }

    // CR 303.4: Aura makes a permanent modified only if controlled by the
    // permanent's controller.
    #[test]
    fn modified_aura_requires_same_controller_as_permanent() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );

        // Creature A: P0 creature with P0 Aura → modified.
        let cre_a = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura_a = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Own Aura".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&aura_a).unwrap();
            a.card_types.core_types.push(CoreType::Enchantment);
            a.card_types.subtypes.push("Aura".into());
            a.attached_to = Some(cre_a.into());
        }
        state
            .objects
            .get_mut(&cre_a)
            .unwrap()
            .attachments
            .push(aura_a);

        // Creature B: P0 creature with P1 Aura → NOT modified.
        let cre_b = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Ox".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let aura_b = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Opp Aura".into(),
            Zone::Battlefield,
        );
        {
            let a = state.objects.get_mut(&aura_b).unwrap();
            a.card_types.core_types.push(CoreType::Enchantment);
            a.card_types.subtypes.push("Aura".into());
            a.attached_to = Some(cre_b.into());
        }
        state
            .objects
            .get_mut(&cre_b)
            .unwrap()
            .attachments
            .push(aura_b);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Modified]));
        assert!(
            matches_target_filter(&state, cre_a, &filter, source),
            "own-controller aura makes creature modified"
        );
        assert!(
            !matches_target_filter(&state, cre_b, &filter, source),
            "opposing-controller aura does not make creature modified"
        );
    }

    // CR 700.9: Vanilla creature (no counters, no attachments) is not modified.
    #[test]
    fn modified_does_not_match_vanilla_creature() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let cre = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cre)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Modified]));
        assert!(!matches_target_filter(&state, cre, &filter, source));
    }

    // CR 700.6: An object is historic if it has the legendary supertype, the
    // artifact card type, or the Saga subtype.
    #[test]
    fn historic_matches_legendary_creature() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let obj = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Captain".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&obj).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.card_types.supertypes.push(Supertype::Legendary);
        }
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Historic]));
        assert!(matches_target_filter(&state, obj, &filter, source));
    }

    #[test]
    fn historic_matches_artifact() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let obj = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bauble".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Historic]));
        assert!(matches_target_filter(&state, obj, &filter, source));
    }

    #[test]
    fn historic_matches_saga() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let obj = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "History of Benalia".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&obj).unwrap();
            o.card_types.core_types.push(CoreType::Enchantment);
            o.card_types.subtypes.push("Saga".into());
        }
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Historic]));
        assert!(matches_target_filter(&state, obj, &filter, source));
    }

    #[test]
    fn historic_does_not_match_vanilla_creature() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let obj = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Historic]));
        assert!(!matches_target_filter(&state, obj, &filter, source));
    }

    #[test]
    fn historic_does_not_match_basic_land() {
        use crate::types::ability::TypedFilter;
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let obj = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Plains".into(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&obj).unwrap();
            o.card_types.core_types.push(CoreType::Land);
            o.card_types.supertypes.push(Supertype::Basic);
            o.card_types.subtypes.push("Plains".into());
        }
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Historic]));
        assert!(!matches_target_filter(&state, obj, &filter, source));
    }

    #[test]
    fn lki_snapshot_filter_matches_cmc_property() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let lki = crate::types::game_state::LKISnapshot {
            name: "Returned Creature".into(),
            power: Some(2),
            toughness: Some(2),
            mana_value: 3,
            controller: PlayerId(1),
            owner: PlayerId(1),
            card_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            counters: Default::default(),
        };
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            }]));

        assert!(matches_target_filter_on_lki_snapshot(
            &state,
            ObjectId(700),
            &lki,
            &filter,
            &FilterContext::from_source(&state, source),
        ));
    }

    #[test]
    fn lki_snapshot_filter_matches_nonbasic_land_property() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let mut lki = crate::types::game_state::LKISnapshot {
            name: "Destroyed Land".into(),
            power: None,
            toughness: None,
            mana_value: 0,
            controller: PlayerId(1),
            owner: PlayerId(1),
            card_types: vec![CoreType::Land],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            counters: Default::default(),
        };
        let filter =
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::NotSupertype {
                    value: Supertype::Basic,
                }]),
            );
        let ctx = FilterContext::from_source(&state, source);

        assert!(matches_target_filter_on_lki_snapshot(
            &state,
            ObjectId(701),
            &lki,
            &filter,
            &ctx,
        ));

        lki.supertypes.push(Supertype::Basic);
        assert!(!matches_target_filter_on_lki_snapshot(
            &state,
            ObjectId(701),
            &lki,
            &filter,
            &ctx,
        ));
    }

    /// CR 700.6: `FilterProp::Historic` on a zone-change snapshot must read
    /// the captured supertypes / core_types / subtypes — the path used by
    /// Arbaaz Mir's "another nontoken historic permanent enters" trigger.
    /// Each leg (legendary, artifact, Saga) is independently sufficient.
    #[test]
    fn zone_change_record_historic_matches_each_leg() {
        use crate::types::game_state::ZoneChangeRecord;

        let state = GameState::default();
        let source_ctx = SourceContext {
            id: ObjectId(1),
            controller: Some(PlayerId(0)),
            attached_to: None,
            source_is_aura: false,
            source_is_equipment: false,
            chosen_creature_type: None,
            chosen_attributes: &[],
            ability: None,
            recipient_id: None,
        };

        // Leg 1: legendary creature (Arbaaz Mir, In Garruk's Wake-style ETB).
        let legendary_record = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            supertypes: vec![Supertype::Legendary],
            ..ZoneChangeRecord::test_minimal(ObjectId(42), Some(Zone::Library), Zone::Battlefield)
        };
        assert!(zone_change_record_matches_property(
            &FilterProp::Historic,
            &state,
            &legendary_record,
            &source_ctx,
        ));

        // Leg 2: non-legendary artifact (e.g. Sol Ring entering).
        let artifact_record = ZoneChangeRecord {
            core_types: vec![CoreType::Artifact],
            ..ZoneChangeRecord::test_minimal(ObjectId(43), Some(Zone::Hand), Zone::Battlefield)
        };
        assert!(zone_change_record_matches_property(
            &FilterProp::Historic,
            &state,
            &artifact_record,
            &source_ctx,
        ));

        // Leg 3: Saga (non-legendary subtype path — Sagas are typically also
        // Legendary but the predicate matches on the Saga subtype alone).
        let saga_record = ZoneChangeRecord {
            core_types: vec![CoreType::Enchantment],
            subtypes: vec!["Saga".into()],
            ..ZoneChangeRecord::test_minimal(ObjectId(44), Some(Zone::Hand), Zone::Battlefield)
        };
        assert!(zone_change_record_matches_property(
            &FilterProp::Historic,
            &state,
            &saga_record,
            &source_ctx,
        ));

        // Negative: vanilla non-historic creature.
        let vanilla_record = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            ..ZoneChangeRecord::test_minimal(ObjectId(45), Some(Zone::Hand), Zone::Battlefield)
        };
        assert!(!zone_change_record_matches_property(
            &FilterProp::Historic,
            &state,
            &vanilla_record,
            &source_ctx,
        ));
    }

    /// CR 700.6: `FilterProp::Historic` on a `SpellCastRecord` must read the
    /// cast-time card-type snapshot — the path used by Jhoira, Weatherlight
    /// Captain's "whenever you cast a historic spell" trigger.
    #[test]
    fn spell_record_historic_matches_each_leg() {
        use crate::types::game_state::SpellCastRecord;

        let make_record = |core_types: Vec<CoreType>,
                           supertypes: Vec<Supertype>,
                           subtypes: Vec<String>|
         -> SpellCastRecord {
            SpellCastRecord {
                core_types,
                supertypes,
                subtypes,
                keywords: Vec::new(),
                colors: Vec::new(),
                mana_value: 0,
                has_x_in_cost: false,
            }
        };

        // Leg 1: legendary creature spell.
        let legendary_record =
            make_record(vec![CoreType::Creature], vec![Supertype::Legendary], vec![]);
        assert!(spell_record_matches_property(
            &legendary_record,
            &FilterProp::Historic,
        ));

        // Leg 2: non-legendary artifact spell.
        let artifact_record = make_record(vec![CoreType::Artifact], vec![], vec![]);
        assert!(spell_record_matches_property(
            &artifact_record,
            &FilterProp::Historic,
        ));

        // Leg 3: Saga spell (legendary enchantment subtype).
        let saga_record = make_record(
            vec![CoreType::Enchantment],
            vec![Supertype::Legendary],
            vec!["Saga".into()],
        );
        assert!(spell_record_matches_property(
            &saga_record,
            &FilterProp::Historic,
        ));

        // Negative: vanilla creature spell.
        let vanilla_record = make_record(vec![CoreType::Creature], vec![], vec![]);
        assert!(!spell_record_matches_property(
            &vanilla_record,
            &FilterProp::Historic,
        ));
    }

    /// CR 111.1: `FilterProp::Token` on a zone-change snapshot must read the
    /// captured `is_token` bit, not the live battlefield state (which no longer
    /// exists once the token has moved to the graveyard). Grismold-style
    /// "whenever a creature token dies" triggers depend on this.
    #[test]
    fn zone_change_record_token_property_matches_snapshot() {
        let state = GameState::default();
        let source_ctx = SourceContext {
            id: ObjectId(1),
            controller: Some(PlayerId(0)),
            attached_to: None,
            source_is_aura: false,
            source_is_equipment: false,
            chosen_creature_type: None,
            chosen_attributes: &[],
            ability: None,
            recipient_id: None,
        };

        let token_record = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            is_token: true,
            ..ZoneChangeRecord::test_minimal(ObjectId(42), Some(Zone::Battlefield), Zone::Graveyard)
        };
        assert!(zone_change_record_matches_property(
            &FilterProp::Token,
            &state,
            &token_record,
            &source_ctx,
        ));

        let nontoken_record = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            is_token: false,
            ..ZoneChangeRecord::test_minimal(ObjectId(43), Some(Zone::Battlefield), Zone::Graveyard)
        };
        assert!(!zone_change_record_matches_property(
            &FilterProp::Token,
            &state,
            &nontoken_record,
            &source_ctx,
        ));

        let enchanted_record = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            attachments: vec![AttachmentSnapshot {
                object_id: ObjectId(100),
                controller: PlayerId(0),
                kind: AttachmentKind::Aura,
            }],
            ..ZoneChangeRecord::test_minimal(ObjectId(44), Some(Zone::Battlefield), Zone::Graveyard)
        };
        assert!(zone_change_record_matches_property(
            &FilterProp::HasAnyAttachmentOf {
                kinds: vec![AttachmentKind::Aura, AttachmentKind::Equipment],
                controller: None,
            },
            &state,
            &enchanted_record,
            &source_ctx,
        ));
        assert!(zone_change_record_matches_property(
            &FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: Some(ControllerRef::You),
            },
            &state,
            &enchanted_record,
            &source_ctx,
        ));
        assert!(!zone_change_record_matches_property(
            &FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: None,
            },
            &state,
            &enchanted_record,
            &source_ctx,
        ));
    }

    /// CR 506.4 + CR 603.10a: Combat predicates on a zone-change object read
    /// the event snapshot because live combat state no longer contains objects
    /// that have left combat.
    #[test]
    fn zone_change_record_combat_properties_match_snapshot() {
        use crate::types::game_state::{ZoneChangeCombatStatus, ZoneChangeRecord};

        let state = GameState::default();
        let source_ctx = SourceContext {
            id: ObjectId(1),
            controller: Some(PlayerId(0)),
            attached_to: None,
            source_is_aura: false,
            source_is_equipment: false,
            chosen_creature_type: None,
            chosen_attributes: &[],
            ability: None,
            recipient_id: None,
        };
        let attacking_record = ZoneChangeRecord {
            combat_status: ZoneChangeCombatStatus {
                attacking: true,
                blocking: false,
                blocked: false,
                defending_player: Some(PlayerId(0)),
            },
            ..ZoneChangeRecord::test_minimal(ObjectId(42), Some(Zone::Battlefield), Zone::Graveyard)
        };
        let blocking_record = ZoneChangeRecord {
            combat_status: ZoneChangeCombatStatus {
                attacking: false,
                blocking: true,
                blocked: false,
                defending_player: None,
            },
            ..ZoneChangeRecord::test_minimal(ObjectId(43), Some(Zone::Battlefield), Zone::Graveyard)
        };

        assert!(zone_change_record_matches_property(
            &FilterProp::Attacking,
            &state,
            &attacking_record,
            &source_ctx,
        ));
        assert!(zone_change_record_matches_property(
            &FilterProp::Unblocked,
            &state,
            &attacking_record,
            &source_ctx,
        ));
        assert!(zone_change_record_matches_property(
            &FilterProp::AttackingController,
            &state,
            &attacking_record,
            &source_ctx,
        ));
        assert!(zone_change_record_matches_property(
            &FilterProp::Blocking,
            &state,
            &blocking_record,
            &source_ctx,
        ));
    }

    // ===========================================================================
    // CR 702.73a — Changeling subtype expansion cascade.
    //
    // These tests pin the single-authority `subtype_matches_with_changeling`
    // helper across every public consumer: on-battlefield filters, library/hand
    // filters (SearchLibrary / RevealFromHand), spell-cast snapshots
    // (ReduceCost on stack), and zone-change snapshots. They also pin the
    // CR 205.3m gate — a Changeling object must NOT match non-creature subtypes.
    // ===========================================================================

    fn add_changeling_in_zone(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        zone: Zone,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            zone,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        // Printed subtype is something narrow; Changeling must expand the rest.
        obj.card_types.subtypes.push("Illusion".to_string());
        obj.keywords.push(Keyword::Changeling);
        id
    }

    fn make_subtype_filter(subtype: &str) -> TargetFilter {
        TargetFilter::Typed(TypedFilter::card().with_type(TypeFilter::Subtype(subtype.to_string())))
    }

    /// CR 702.73a: A Changeling object on the battlefield matches every
    /// creature-subtype filter in `state.all_creature_types` — covers
    /// target-legality and static-affected cascade for tribal lords
    /// ("Goblins you control get +1/+1") via the same code path.
    #[test]
    fn changeling_battlefield_matches_every_creature_subtype() {
        let mut state = setup();
        state.all_creature_types = vec![
            "Elf".to_string(),
            "Goblin".to_string(),
            "Dragon".to_string(),
        ];
        let id = add_changeling_in_zone(
            &mut state,
            PlayerId(0),
            "Mistform Ultimus",
            Zone::Battlefield,
        );

        for subtype in ["Elf", "Goblin", "Dragon", "Illusion"] {
            assert!(
                matches_target_filter(&state, id, &make_subtype_filter(subtype), id),
                "Changeling battlefield object should match Subtype({subtype})",
            );
        }
    }

    /// CR 702.73a + CR 205.3m: Changeling confers only creature subtypes — it
    /// must NOT match non-creature subtypes (artifact / land / enchantment
    /// types). The runtime catalog `state.all_creature_types` is the gate.
    #[test]
    fn changeling_does_not_match_non_creature_subtypes() {
        let mut state = setup();
        // Catalog only contains creature subtypes (per deck-loading), so
        // Plains/Equipment/Aura are absent and must not match.
        state.all_creature_types = vec!["Elf".to_string()];
        let id = add_changeling_in_zone(
            &mut state,
            PlayerId(0),
            "Mistform Ultimus",
            Zone::Battlefield,
        );

        for non_creature in ["Plains", "Equipment", "Aura", "Saga"] {
            assert!(
                !matches_target_filter(&state, id, &make_subtype_filter(non_creature), id),
                "Changeling must NOT match non-creature subtype {non_creature}",
            );
        }
    }

    /// CR 702.73a: Library cascade (Gilt-Leaf Palace search). A Changeling card
    /// in the library matches `Subtype: Elf` even though the layer system
    /// doesn't run on non-battlefield zones — the keyword carries through and
    /// the filter helper does the expansion at evaluation time.
    #[test]
    fn changeling_in_library_matches_subtype_filter() {
        let mut state = setup();
        state.all_creature_types = vec!["Elf".to_string(), "Treefolk".to_string()];
        let id = add_changeling_in_zone(&mut state, PlayerId(0), "Mistform Ultimus", Zone::Library);

        assert!(matches_target_filter(
            &state,
            id,
            &make_subtype_filter("Elf"),
            id
        ));
        assert!(matches_target_filter(
            &state,
            id,
            &make_subtype_filter("Treefolk"),
            id
        ));
        // Library card must still gate — Plains is not a creature type.
        assert!(!matches_target_filter(
            &state,
            id,
            &make_subtype_filter("Plains"),
            id
        ));
    }

    /// CR 702.73a: Hand cascade (RevealFromHand). Equivalent to the library
    /// case — same code path, different zone, same expected behavior.
    #[test]
    fn changeling_in_hand_matches_subtype_filter() {
        let mut state = setup();
        state.all_creature_types = vec!["Soldier".to_string()];
        let id = add_changeling_in_zone(&mut state, PlayerId(0), "Mistform Ultimus", Zone::Hand);

        assert!(matches_target_filter(
            &state,
            id,
            &make_subtype_filter("Soldier"),
            id
        ));
        // The card's printed subtype still matches.
        assert!(matches_target_filter(
            &state,
            id,
            &make_subtype_filter("Illusion"),
            id
        ));
    }

    /// CR 702.73a: Stack cascade (Ur-Dragon ReduceCost). Spell-record snapshots
    /// must honour Changeling — `Subtype: Dragon` matches Mistform Ultimus on
    /// the stack via `spell_record_matches_filter`.
    #[test]
    fn changeling_spell_record_matches_subtype_filter() {
        let all_creature_types = vec!["Dragon".to_string(), "Goblin".to_string()];
        let record = SpellCastRecord {
            core_types: vec![CoreType::Creature],
            supertypes: vec![],
            subtypes: vec!["Illusion".to_string()],
            keywords: vec![Keyword::Changeling],
            colors: vec![],
            mana_value: 7,
            has_x_in_cost: false,
        };
        let dragon_filter = make_subtype_filter("Dragon");
        let plains_filter = make_subtype_filter("Plains");

        assert!(spell_record_matches_filter(
            &record,
            &dragon_filter,
            PlayerId(0),
            &all_creature_types,
        ));
        // CR 205.3m gate: non-creature subtype must NOT match.
        assert!(!spell_record_matches_filter(
            &record,
            &plains_filter,
            PlayerId(0),
            &all_creature_types,
        ));
        // No catalog ⇒ no expansion (still falls back to printed subtypes).
        assert!(!spell_record_matches_filter(
            &record,
            &dragon_filter,
            PlayerId(0),
            &[],
        ));
    }

    /// CR 702.73a + CR 603.10: Zone-change snapshots carry keywords forward,
    /// so look-back triggers ("when a Goblin dies, ...") see Changeling
    /// objects via the same expansion. Pins the third subtype-match site.
    #[test]
    fn changeling_zone_change_record_matches_subtype_filter() {
        let all_creature_types = vec!["Goblin".to_string()];
        let record = ZoneChangeRecord {
            object_id: ObjectId(99),
            name: "Mistform Ultimus".to_string(),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Illusion".to_string()],
            supertypes: vec![],
            keywords: vec![Keyword::Changeling],
            power: Some(2),
            toughness: Some(3),
            colors: vec![],
            mana_value: 5,
            controller: PlayerId(0),
            owner: PlayerId(0),
            from_zone: Some(Zone::Battlefield),
            to_zone: Zone::Graveyard,
            attachments: vec![],
            linked_exile_snapshot: vec![],
            is_token: false,
            combat_status: Default::default(),
        };
        let goblin_filter = make_subtype_filter("Goblin");
        let plains_filter = make_subtype_filter("Plains");

        assert!(zone_change_record_matches_type_filter(
            &record,
            &TypeFilter::Subtype("Goblin".to_string()),
            &all_creature_types,
        ));
        // CR 205.3m gate.
        assert!(!zone_change_record_matches_type_filter(
            &record,
            &TypeFilter::Subtype("Plains".to_string()),
            &all_creature_types,
        ));
        // Sanity: positive cascade through the public TargetFilter API.
        // (Use the type-filter level here since ZoneChangeRecord doesn't expose
        // a public TargetFilter matcher with a free creature-types slice.)
        let _ = (goblin_filter, plains_filter); // referenced for test cohesion
    }

    /// CR 702.73a: Non-Changeling object must NOT pick up creature-type
    /// expansion — the helper short-circuits when the keyword is absent.
    /// Guards against the helper "leaking" expansion to unrelated objects.
    #[test]
    fn non_changeling_does_not_expand_subtypes() {
        let mut state = setup();
        state.all_creature_types = vec![
            "Elf".to_string(),
            "Goblin".to_string(),
            "Dragon".to_string(),
        ];
        // Vanilla bear: Creature — Bear, no keywords.
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Bear".to_string());

        assert!(matches_target_filter(
            &state,
            id,
            &make_subtype_filter("Bear"),
            id
        ));
        for other in ["Elf", "Goblin", "Dragon"] {
            assert!(
                !matches_target_filter(&state, id, &make_subtype_filter(other), id),
                "Non-changeling Bear must NOT match Subtype({other})",
            );
        }
    }
}

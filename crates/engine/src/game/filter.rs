//! Typed object filter matching using TargetFilter enum.
//!
//! Replaces the Forge-style string filter parsing with typed enum matching.
//! All filter logic works against the TargetFilter enum hierarchy from types/ability.rs.

use std::collections::HashSet;

use crate::game::combat;
use crate::game::game_object::GameObject;
use crate::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
use crate::types::ability::{
    ChosenAttribute, ControllerRef, FilterProp, QuantityExpr, ResolvedAbility, SharedQuality,
    TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::game_state::{GameState, SpellCastRecord};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
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
    )
}

fn filter_inner(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    ability: Option<&ResolvedAbility>,
) -> bool {
    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::Player => false,     // Players are not objects
        TargetFilter::Controller => false, // Controller is a player, not an object
        TargetFilter::SelfRef => object_id == source_id,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            properties,
        }) => {
            let obj = match state.objects.get(&object_id) {
                Some(o) => o,
                None => return false,
            };
            // Type filters check (all must match — conjunction)
            for tf in type_filters {
                if !type_filter_matches(tf, obj) {
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
                }
            }
            // All properties must match
            let source_obj = state.objects.get(&source_id);
            let source_attached_to = source_obj.and_then(|s| s.attached_to);
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
                chosen_creature_type: source_chosen_creature_type.as_deref(),
                chosen_attributes: source_chosen_attributes,
                ability,
            };
            properties
                .iter()
                .all(|p| matches_filter_prop(p, state, obj, object_id, &source_ctx))
        }
        TargetFilter::Not { filter: inner } => !filter_inner(
            state,
            object_id,
            inner,
            source_id,
            source_controller,
            ability,
        ),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|f| filter_inner(state, object_id, f, source_id, source_controller, ability)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|f| filter_inner(state, object_id, f, source_id, source_controller, ability)),
        // StackAbility/StackSpell targeting is handled directly at call sites, not via filter
        TargetFilter::StackAbility | TargetFilter::StackSpell => false,
        TargetFilter::SpecificObject { id: target_id } => object_id == *target_id,
        TargetFilter::AttachedTo => state
            .objects
            .get(&source_id)
            .and_then(|src| src.attached_to)
            .is_some_and(|attached| attached == object_id),
        TargetFilter::LastCreated => state.last_created_token_ids.contains(&object_id),
        // CR 603.7: Match objects in a tracked set from the originating effect.
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .is_some_and(|set| set.contains(&object_id)),
        // CR 607.2a + CR 406.6: Match cards exiled by source via exile-until-leaves links.
        // CR 610.3: Linked abilities track which cards were exiled by the first ability.
        TargetFilter::ExiledBySource => state
            .exile_links
            .iter()
            .any(|link| link.source_id == source_id && link.exiled_id == object_id),
        // CR 603.7c: Event-context references resolve to players, not objects.
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::DefendingPlayer => false,
        // ParentTarget/ParentTargetController resolve at resolution time, not via object matching.
        TargetFilter::ParentTarget | TargetFilter::ParentTargetController => false,
        // "card with the chosen name" — match against source's ChosenAttribute::CardName.
        TargetFilter::HasChosenName => {
            let chosen_name = state.objects.get(&source_id).and_then(|obj| {
                obj.chosen_attributes.iter().find_map(|a| match a {
                    ChosenAttribute::CardName(n) => Some(n.as_str()),
                    _ => None,
                })
            });
            chosen_name.is_some_and(|name| {
                state
                    .objects
                    .get(&object_id)
                    .is_some_and(|obj| obj.name == name)
            })
        }
        // "card named [literal]" — static name match.
        TargetFilter::Named { name } => state
            .objects
            .get(&object_id)
            .is_some_and(|obj| obj.name == *name),
    }
}

/// Check if an object matches a TypeFilter variant.
/// Check if an object's card types match a `TypeFilter`.
/// CR 205.2a: Each card type has its own rules for how it behaves.
/// Public for use by trigger_matchers and other modules that need type checking.
pub fn type_filter_matches(tf: &TypeFilter, obj: &GameObject) -> bool {
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
        TypeFilter::Non(inner) => !type_filter_matches(inner, obj),
        // CR 205.3: Subtype matching — Changeling (CR 702.73) types are expanded
        // by the layer system before this check, so obj.card_types.subtypes is complete.
        TypeFilter::Subtype(ref sub) => obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(sub)),
        // CR 608.2b: Disjunction — matches if any inner filter matches.
        TypeFilter::AnyOf(ref filters) => filters.iter().any(|f| type_filter_matches(f, obj)),
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
                }
            }

            type_filters
                .iter()
                .all(|type_filter| spell_record_matches_type_filter(record, type_filter))
                && properties
                    .iter()
                    .all(|prop| spell_record_matches_property(record, prop))
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|inner| spell_record_matches_filter(record, inner, controller)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|inner| spell_record_matches_filter(record, inner, controller)),
        TargetFilter::Not { filter: inner } => {
            !spell_record_matches_filter(record, inner, controller)
        }
        // All remaining variants are inapplicable to spell snapshots.
        TargetFilter::None
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::StackAbility
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. } => false,
    }
}

/// Check whether a spell object being cast matches a target filter.
///
/// Unlike [`spell_record_matches_filter`], this preserves the spell's current zone
/// and interprets `ControllerRef` relative to the current caster rather than the
/// object's stored controller.
pub fn spell_object_matches_filter(
    spell_obj: &GameObject,
    caster: PlayerId,
    filter: &TargetFilter,
    source_controller: PlayerId,
) -> bool {
    let record = SpellCastRecord {
        core_types: spell_obj.card_types.core_types.clone(),
        supertypes: spell_obj.card_types.supertypes.clone(),
        subtypes: spell_obj.card_types.subtypes.clone(),
        keywords: spell_obj.keywords.clone(),
        colors: spell_obj.color.clone(),
        mana_value: spell_obj.mana_cost.mana_value(),
    };
    spell_object_matches_filter_inner(&record, spell_obj.zone, caster, filter, source_controller)
}

fn spell_object_matches_filter_inner(
    record: &SpellCastRecord,
    zone: Zone,
    caster: PlayerId,
    filter: &TargetFilter,
    source_controller: PlayerId,
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
                    _ => {}
                }
            }

            type_filters
                .iter()
                .all(|type_filter| spell_record_matches_type_filter(record, type_filter))
                && properties
                    .iter()
                    .all(|prop| spell_object_matches_property(record, zone, prop))
        }
        TargetFilter::Or { filters } => filters.iter().any(|inner| {
            spell_object_matches_filter_inner(record, zone, caster, inner, source_controller)
        }),
        TargetFilter::And { filters } => filters.iter().all(|inner| {
            spell_object_matches_filter_inner(record, zone, caster, inner, source_controller)
        }),
        TargetFilter::Not { filter: inner } => {
            !spell_object_matches_filter_inner(record, zone, caster, inner, source_controller)
        }
        TargetFilter::None
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::StackAbility
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetController
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. } => false,
    }
}

fn spell_object_matches_property(record: &SpellCastRecord, zone: Zone, prop: &FilterProp) -> bool {
    match prop {
        FilterProp::InZone { zone: required } => zone == *required,
        FilterProp::InAnyZone { zones } => zones.contains(&zone),
        _ => spell_record_matches_property(record, prop),
    }
}

fn spell_record_matches_type_filter(record: &SpellCastRecord, filter: &TypeFilter) -> bool {
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
        TypeFilter::Non(inner) => !spell_record_matches_type_filter(record, inner),
        TypeFilter::Subtype(subtype) => record
            .subtypes
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(subtype)),
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .any(|inner| spell_record_matches_type_filter(record, inner)),
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
        FilterProp::Multicolored => record.colors.len() > 1,
        // CR 105.2c: Colorless objects have no color.
        FilterProp::Colorless => record.colors.is_empty(),
        FilterProp::CmcGE { value } => match value {
            QuantityExpr::Fixed { value } => record.mana_value as i32 >= *value,
            _ => {
                debug_assert!(false, "dynamic QuantityExpr in spell record CmcGE filter — parser should only produce Fixed values here");
                false
            }
        },
        FilterProp::CmcLE { value } => match value {
            QuantityExpr::Fixed { value } => (record.mana_value as i32) <= *value,
            _ => {
                debug_assert!(false, "dynamic QuantityExpr in spell record CmcLE filter — parser should only produce Fixed values here");
                false
            }
        },
        FilterProp::CmcEQ { value } => match value {
            QuantityExpr::Fixed { value } => record.mana_value as i32 == *value,
            _ => {
                debug_assert!(false, "dynamic QuantityExpr in spell record CmcEQ filter — parser should only produce Fixed values here");
                false
            }
        },
        // All remaining props require on-battlefield or stack state unavailable from a snapshot.
        FilterProp::Token
        | FilterProp::Attacking
        | FilterProp::Blocking
        | FilterProp::Unblocked
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::CountersGE { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::Another
        | FilterProp::PowerLE { .. }
        | FilterProp::PowerGE { .. }
        | FilterProp::PowerGTSource
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::HasSingleTarget
        | FilterProp::Suspected
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
        | FilterProp::Other { .. } => false,
    }
}

/// Context about the source of an ability, used during filter property evaluation.
struct SourceContext<'a> {
    id: ObjectId,
    controller: Option<PlayerId>,
    attached_to: Option<ObjectId>,
    chosen_creature_type: Option<&'a str>,
    chosen_attributes: &'a [crate::types::ability::ChosenAttribute],
    /// CR 107.3a + CR 601.2b: The resolving ability, when one is in scope.
    /// Dynamic filter thresholds (`QuantityRef::Variable { "X" }`, `TargetPower`, etc.)
    /// resolve against this ability's `chosen_x` and `targets`. `None` for contexts
    /// without a resolving ability (combat restrictions, layer predicates); in that
    /// case, per CR 107.2, any `Variable("X")` fallback resolves to 0.
    ability: Option<&'a ResolvedAbility>,
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

/// Check if an object satisfies a single FilterProp.
fn matches_filter_prop(
    prop: &FilterProp,
    state: &GameState,
    obj: &GameObject,
    object_id: ObjectId,
    source: &SourceContext<'_>,
) -> bool {
    match prop {
        FilterProp::Token => {
            // A token has no card_id (card_id.0 == 0) in typical token creation
            // For now, permissive true -- tokens will be marked more explicitly later
            true
        }
        FilterProp::Attacking => state.combat.as_ref().is_some_and(|combat| {
            combat
                .attackers
                .iter()
                .any(|attacker| attacker.object_id == object_id)
        }),
        // CR 509.1a: A creature is blocking if it was declared as a blocker.
        FilterProp::Blocking => state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&object_id)),
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
        // CR 202.3: Mana value threshold comparisons. Dynamic thresholds
        // (`QuantityRef::Variable { "X" }`) resolve against the ability's
        // `chosen_x` when a `ResolvedAbility` is in scope via `FilterContext::from_ability`.
        FilterProp::CmcGE { value } => {
            let cmc = obj.mana_cost.mana_value() as i32;
            cmc >= resolve_filter_threshold(state, value, source)
        }
        FilterProp::CmcLE { value } => {
            let cmc = obj.mana_cost.mana_value() as i32;
            cmc <= resolve_filter_threshold(state, value, source)
        }
        FilterProp::CmcEQ { value } => {
            let cmc = obj.mana_cost.mana_value() as i32;
            cmc == resolve_filter_threshold(state, value, source)
        }
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
        FilterProp::InZone { zone } => obj.zone == *zone,
        FilterProp::Owned { controller } => match controller {
            ControllerRef::You => source.controller == Some(obj.owner),
            ControllerRef::Opponent => {
                source.controller.is_some() && source.controller != Some(obj.owner)
            }
        },
        FilterProp::EnchantedBy => source.attached_to == Some(object_id),
        FilterProp::EquippedBy => source.attached_to == Some(object_id),
        FilterProp::Another => object_id != source.id,
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
        // CR 701.60b: Match creatures with the suspected designation.
        FilterProp::Suspected => obj.is_suspected,
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
        // CR 601.2c: Group constraint — not evaluable per-object, validated at resolution time.
        FilterProp::SharesQuality { .. } => true,
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
        FilterProp::Other { .. } => false, // Fail-closed for unrecognized properties
    }
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

    match quality {
        SharedQuality::CreatureType => {
            // Collect subtypes for each object, then intersect.
            let mut subtype_sets: Vec<HashSet<&str>> = Vec::new();
            for &id in &obj_ids {
                if let Some(obj) = state.objects.get(&id) {
                    let set: HashSet<&str> =
                        obj.card_types.subtypes.iter().map(|s| s.as_str()).collect();
                    subtype_sets.push(set);
                } else {
                    return false;
                }
            }
            // Intersect all sets — at least one common subtype must exist.
            let mut shared = subtype_sets[0].clone();
            for set in &subtype_sets[1..] {
                shared = shared.intersection(set).copied().collect();
            }
            !shared.is_empty()
        }
        SharedQuality::Color => {
            // All objects must share at least one color.
            let mut color_sets: Vec<HashSet<&ManaColor>> = Vec::new();
            for &id in &obj_ids {
                if let Some(obj) = state.objects.get(&id) {
                    let set: HashSet<&ManaColor> = obj.color.iter().collect();
                    color_sets.push(set);
                } else {
                    return false;
                }
            }
            let mut shared = color_sets[0].clone();
            for set in &color_sets[1..] {
                shared = shared.intersection(set).copied().collect();
            }
            !shared.is_empty()
        }
        SharedQuality::CardType => {
            // All objects must share at least one core card type.
            let mut type_sets: Vec<HashSet<&CoreType>> = Vec::new();
            for &id in &obj_ids {
                if let Some(obj) = state.objects.get(&id) {
                    let set: HashSet<&CoreType> = obj.card_types.core_types.iter().collect();
                    type_sets.push(set);
                } else {
                    return false;
                }
            }
            let mut shared = type_sets[0].clone();
            for set in &type_sets[1..] {
                shared = shared.intersection(set).copied().collect();
            }
            !shared.is_empty()
        }
    }
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
        TargetFilter::Typed(ref tf) if tf.type_filters.is_empty() => match &tf.controller {
            Some(ControllerRef::You) => source_controller == Some(player_id),
            Some(ControllerRef::Opponent) => source_controller.is_some_and(|c| c != player_id),
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
    use crate::types::ability::{ChosenAttribute, ControllerRef, FilterProp, TargetFilter};
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
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
        assert!(spell_record_matches_filter(&record, &filter, PlayerId(0)));
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
        state.objects.get_mut(&aura).unwrap().attached_to = Some(creature_a);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));

        assert!(matches_target_filter(&state, creature_a, &filter, aura));
        assert!(
            !matches_target_filter(&state, creature_b, &filter, aura),
            "EnchantedBy must not match creatures the aura is NOT attached to"
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
    fn exiled_by_source_matches_linked_objects() {
        use crate::types::game_state::ExileLink;

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
            return_zone: Zone::Battlefield,
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
        ));
        assert!(!spell_object_matches_filter(
            spell,
            PlayerId(1),
            &filter,
            PlayerId(0),
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
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
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
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
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
        };
        let json = serde_json::to_string(&search).unwrap();
        let restored: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(search, restored);
    }
}

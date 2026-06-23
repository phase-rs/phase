//! Typed object filter matching using TargetFilter enum.
//!
//! Replaces the Forge-style string filter parsing with typed enum matching.
//! All filter logic works against the TargetFilter enum hierarchy from types/ability.rs.

use std::collections::{HashMap, HashSet};

use crate::game::combat;
use crate::game::game_object::GameObject;
use crate::game::quantity::{
    counter_count_from_map, resolve_quantity, resolve_quantity_with_targets,
};
use crate::types::ability::{
    ChoiceValue, ChosenAttribute, CombatRelation, CombatRelationSubject, ControllerRef, CountScope,
    FilterProp, Parity, ParitySource, PtStat, PtValueScope, QuantityExpr, ResolvedAbility,
    SharedQuality, SharedQualityRelation, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterMatch;
use crate::types::game_state::{
    AttackDeclarationRecord, CounterAddedRecord, DamageRecord, GameState, LKISnapshot,
    SpellCastRecord, StackEntryKind, ZoneChangeRecord,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;
use crate::types::proposed_event::{EtbTapState, ProposedEvent, TokenSpec};
use crate::types::zones::Zone;

/// True when the filter's matched SET depends on the population of objects on
/// the battlefield — i.e. another object entering or leaving the battlefield can
/// change whether a PRE-EXISTING object satisfies this filter.
///
/// CR 611.3a: a static-ability continuous effect applies at any moment to
/// whatever its text indicates; if its affected set is defined by board
/// population ("creatures that share a name with another permanent", "with the
/// most counters", etc.) then an entry/exit changes which pre-existing objects
/// it affects. The incremental layer-flush fast path must escalate to a full
/// re-evaluation in that case. CR 613.7d: the entering object receives its
/// timestamp on zone entry, so even a fixed affected-set can reorder.
///
/// Sibling of the fail-closed exhaustive FilterProp match in this module — it
/// answers a DIFFERENT question (population dependence, not membership), so it is
/// built as a distinct recursion rather than overloading that match.
pub(crate) fn affected_filter_uses_object_population(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Not { filter: inner } => affected_filter_uses_object_population(inner),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(affected_filter_uses_object_population)
        }
        TargetFilter::Typed(TypedFilter { properties, .. }) => {
            properties.iter().any(filter_prop_uses_object_population)
        }
        // No other TargetFilter arm defines its set by whole-board population.
        // Self/source/target/parent/triggering/specific references resolve to a
        // fixed object or player; zone/exile/tracked-set references read a
        // specific zone or ledger, not battlefield membership.
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
}

/// EXHAUSTIVE, wildcard-free leaf classifier for
/// `affected_filter_uses_object_population`. Adding a `FilterProp` variant forces
/// a decision here. `true` only for props whose membership reads whole-board
/// object population; recurses into embedded `QuantityExpr` thresholds and inner
/// filters.
fn filter_prop_uses_object_population(prop: &FilterProp) -> bool {
    match prop {
        // Structurally board-population dependent.
        FilterProp::MostPrevalentCreatureTypeIn { .. }
        | FilterProp::NameMatchesAnyPermanent { .. } => true,
        // Membership depends on the names of every battlefield permanent matched
        // by the inner filter ("with a different name than each X you control"),
        // so any entry/exit of a matching permanent can flip membership for a
        // pre-existing object. Unconditionally population dependent.
        FilterProp::DifferentNameFrom { .. } => true,
        // CR 603.4: "shares a quality with" a reference set is population
        // dependent ONLY when a reference filter is present — the reference set
        // is battlefield-derived. The multi-target group-share form
        // (`reference = None`) is candidate-local, validated at resolution time,
        // not whole-board membership.
        FilterProp::SharesQuality { reference, .. } => reference.is_some(),
        // Embedded-threshold props: population dependent iff the threshold
        // expression reads object count.
        FilterProp::Counters { count, .. } => {
            crate::game::quantity::quantity_expr_uses_object_count(count)
        }
        FilterProp::Cmc { value, .. } => {
            crate::game::quantity::quantity_expr_uses_object_count(value)
        }
        FilterProp::PtComparison { value, .. } => {
            crate::game::quantity::quantity_expr_uses_object_count(value)
        }
        // Disjunctive composite: recurse.
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_uses_object_population),
        // CR 608.2c: Negation does not change WHICH game state the inner prop
        // reads, so the population dependency is the inner prop's — recurse.
        FilterProp::Not { prop } => filter_prop_uses_object_population(prop),
        // Intentional leaf-false. These are candidate-local, stack-relative,
        // single-object, or carry no QuantityExpr threshold, so a board entry/exit
        // cannot change whether a pre-existing object satisfies them.
        // `ColorCount` carries a `u8` constant, not a QuantityExpr.
        FilterProp::CanEnchant { .. }
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::WasPlayed
        | FilterProp::Attacking { .. }
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::Untapped
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::WithKeyword { .. }
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeyword { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::HasSingleTarget
        | FilterProp::NotColor { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::InAnyZone { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::HasXInManaCost
        | FilterProp::WasKicked
        | FilterProp::HasXInActivationCost
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::IsCommander
        | FilterProp::Other { .. } => false,
    }
}

/// CR 611.3a: ENTRY-AWARE narrowing for a population-sensitive AFFECTED FILTER.
/// `affected_filter_uses_object_population` proves an effect's affected set *can*
/// read board population; this proves a SPECIFIC entering object can actually
/// perturb that population input (so a pre-existing recipient's membership might
/// change).
///
/// Monotonicity: reached only for battlefield ENTRIES. An entry only ADDS to the
/// board, so the only way it changes a population-derived affected set is if the
/// entered object joins the population the set is computed over — EXCEPT for
/// whole-board TALLY props (most-prevalent / name-matches), which can flip a
/// pre-existing object's membership regardless of whether the entered object
/// matches any inner filter; those escalate unconditionally (MEDIUM-2).
///
/// `ctx` is built from the EFFECT SOURCE (CR 109.5 controller rebinding) by the
/// caller. Mirrors the structural recursion of
/// `affected_filter_uses_object_population`.
pub(crate) fn entered_object_perturbs_affected_filter(
    state: &GameState,
    entered_id: ObjectId,
    ctx: &FilterContext<'_>,
    filter: &TargetFilter,
) -> bool {
    match filter {
        TargetFilter::Not { filter: inner } => {
            entered_object_perturbs_affected_filter(state, entered_id, ctx, inner)
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => filters
            .iter()
            .any(|f| entered_object_perturbs_affected_filter(state, entered_id, ctx, f)),
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties
            .iter()
            .any(|p| entered_object_perturbs_filter_prop(state, entered_id, ctx, p)),
        // Identical enumeration to the `false` arm of
        // `affected_filter_uses_object_population`: these references resolve to a
        // fixed object/player, a specific zone, or a tracked ledger — never
        // whole-board population — so the classifier proved them non-population
        // and an entry cannot perturb them.
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
}

/// CR 611.3a: entry-membership leaf for `entered_object_perturbs_affected_filter`.
/// EXHAUSTIVE and wildcard-free, mirroring `filter_prop_uses_object_population`:
/// every `false` arm there is `false` here; every `true` arm there is narrowed
/// here to a membership / threshold-perturb test — EXCEPT the whole-board tally
/// props, which escalate unconditionally.
fn entered_object_perturbs_filter_prop(
    state: &GameState,
    entered_id: ObjectId,
    ctx: &FilterContext<'_>,
    prop: &FilterProp,
) -> bool {
    match prop {
        // Whole-board tally — any entry can flip a pre-existing object's
        // membership; the entered-object filter match is irrelevant, escalate
        // unconditionally (MEDIUM-2). CR 205.3m (creature types — the tally
        // counts creatures by their shared subtype lists), CR 201.2 (name
        // matches any permanent).
        FilterProp::MostPrevalentCreatureTypeIn { .. } => true,
        FilterProp::NameMatchesAnyPermanent { .. } => true,
        // The entered object's name joins the comparison set, so any entry
        // matching the inner filter changes the "different name than each X"
        // membership for pre-existing objects. No inner filter ⇒ conservatively
        // perturb.
        FilterProp::DifferentNameFrom { filter } => {
            matches_target_filter(state, entered_id, filter, ctx)
        }
        // CR 603.4: the reference set is battlefield-derived only when a
        // reference filter is present (classifier returns false for `None`). The
        // `None` arm is therefore unreachable here, but enumerated as `false`
        // for exhaustiveness rather than `unreachable!`.
        FilterProp::SharesQuality { reference, .. } => reference
            .as_ref()
            .is_some_and(|f| matches_target_filter(state, entered_id, f, ctx)),
        // Embedded thresholds: perturbed iff the entered object can perturb the
        // threshold expression's population input.
        FilterProp::Counters { count, .. } => {
            entered_perturbs_quantity(state, entered_id, ctx, count)
        }
        FilterProp::Cmc { value, .. } => entered_perturbs_quantity(state, entered_id, ctx, value),
        FilterProp::PtComparison { value, .. } => {
            entered_perturbs_quantity(state, entered_id, ctx, value)
        }
        FilterProp::AnyOf { props } => props
            .iter()
            .any(|p| entered_object_perturbs_filter_prop(state, entered_id, ctx, p)),
        // CR 608.2c: Negation reads the same game state as the inner prop, so an
        // entry perturbs the negated prop iff it perturbs the inner — recurse.
        FilterProp::Not { prop } => {
            entered_object_perturbs_filter_prop(state, entered_id, ctx, prop)
        }
        // Identical enumeration to the leaf-false arm of
        // `filter_prop_uses_object_population` — candidate-local, stack-relative,
        // single-object, or threshold-free, so a board entry cannot perturb them.
        FilterProp::CanEnchant { .. }
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::WasPlayed
        | FilterProp::Attacking { .. }
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::Untapped
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::WithKeyword { .. }
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeyword { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::HasSingleTarget
        | FilterProp::NotColor { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::InAnyZone { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::HasXInManaCost
        | FilterProp::WasKicked
        | FilterProp::HasXInActivationCost
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::IsCommander
        | FilterProp::Other { .. } => false,
    }
}

/// Bridge: route an embedded threshold `QuantityExpr` through the quantity
/// module's entry-aware classifier. The entered object is resolved to its
/// `GameObject` (it has just entered, so it must exist); a missing object can't
/// perturb anything.
fn entered_perturbs_quantity(
    state: &GameState,
    entered_id: ObjectId,
    ctx: &FilterContext<'_>,
    expr: &QuantityExpr,
) -> bool {
    state.objects.get(&entered_id).is_some_and(|entered| {
        crate::game::quantity::entered_object_perturbs_quantity_expr(state, entered, ctx, expr)
    })
}

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
    /// CR 120.3: Per-player iteration binding for `DamageEachPlayer` quantity
    /// resolution. Distinct from `source_controller`, which remains the
    /// ability's controller for `ControllerRef::You` ("creatures you control").
    pub scoped_iteration_player: Option<PlayerId>,
}

impl<'a> FilterContext<'a> {
    /// Context-free object matching. Use only for constraints whose filters are
    /// printed object qualities rather than source/controller-relative clauses.
    pub fn neutral() -> Self {
        Self {
            source_id: ObjectId(0),
            source_controller: None,
            ability: None,
            recipient_id: None,
            scoped_iteration_player: None,
        }
    }

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
            scoped_iteration_player: None,
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
            scoped_iteration_player: None,
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
            scoped_iteration_player: None,
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
            scoped_iteration_player: None,
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
            scoped_iteration_player: None,
        }
    }
}

fn scoped_player_or_controller(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
    source_controller: Option<PlayerId>,
    scoped_iteration_player: Option<PlayerId>,
) -> Option<PlayerId> {
    // CR 109.5 + CR 120.3: `ControllerRef::ScopedPlayer` first uses an
    // ability-scoped binding, then the per-player binding from
    // DamageEachPlayer quantity resolution; `source_controller` remains the
    // fallback for "you"/"your" when no scoped player is active.
    ability
        .and_then(|a| a.scoped_player)
        .or(scoped_iteration_player)
        .or_else(|| crate::game::quantity::triggering_event_player(state))
        .or(source_controller)
}

fn parent_target_controller_player(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
) -> Option<PlayerId> {
    ability.and_then(|a| {
        crate::game::targeting::resolve_effect_player_ref(
            state,
            a,
            &TargetFilter::ParentTargetController,
        )
    })
}

fn parent_target_owner_player(
    state: &GameState,
    ability: Option<&ResolvedAbility>,
) -> Option<PlayerId> {
    ability.and_then(|a| {
        crate::game::targeting::resolve_effect_player_ref(
            state,
            a,
            &TargetFilter::ParentTargetOwner,
        )
    })
}

#[derive(Clone, Copy)]
enum ControllerLookup {
    /// Normal filter matching: off-stack/off-battlefield objects may need
    /// at-departure controller information for look-back effects.
    LiveOrLki,
    /// Owner-zone matching has already substituted ownership for controller;
    /// stale LKI must not override that owner-scoped value.
    LiveOnly,
}

/// CR 608.2h + CR 400.7: The effective controller of `obj` for filter
/// predicates that look back at non-battlefield objects.
///
/// On the stack and battlefield, `obj.controller` is the live value. Once an
/// object leaves those zones, it ceases to have a controller (CR 109.4: "Objects
/// that are neither on the stack nor on the battlefield aren't controlled by
/// any player"), and the at-departure controller is preserved in
/// `state.lki_cache` by `change_zone` (`game/zones.rs:65-92`). Filters such as
/// "creatures they controlled that were exiled this way" (Oversimplify) must
/// read the at-exile controller, not the post-reset owner; the LKI cache holds
/// exactly that value.
///
/// Returns the LKI controller when the lookup mode permits it, the object is
/// outside the stack/battlefield, and an LKI snapshot exists; otherwise the
/// live `obj.controller`. Stack and battlefield objects always use the live
/// value.
fn effective_controller(
    state: &GameState,
    obj: &GameObject,
    object_id: ObjectId,
    controller_lookup: ControllerLookup,
) -> PlayerId {
    if matches!(controller_lookup, ControllerLookup::LiveOrLki)
        && !matches!(obj.zone, Zone::Battlefield | Zone::Stack)
    {
        if let Some(lki) = state.lki_cache.get(&object_id) {
            return lki.controller;
        }
    }
    obj.controller
}

pub(crate) fn controller_ref_player(
    state: &GameState,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    ability: Option<&ResolvedAbility>,
    controller: &ControllerRef,
) -> Option<PlayerId> {
    match controller {
        ControllerRef::You => source_controller,
        ControllerRef::Opponent => None,
        ControllerRef::ScopedPlayer => {
            scoped_player_or_controller(state, ability, source_controller, None)
        }
        ControllerRef::TargetPlayer => ability.and_then(|a| {
            a.targets.iter().find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
        }),
        ControllerRef::ParentTargetController => parent_target_controller_player(state, ability),
        ControllerRef::ParentTargetOwner => parent_target_owner_player(state, ability),
        ControllerRef::DefendingPlayer => {
            crate::game::combat::defending_player_for_attacker(state, source_id)
        }
        // CR 608.2c + CR 109.4: The player chosen by the Nth `Choose(Player)`
        // in this resolution — read from the resolution-scoped list.
        ControllerRef::ChosenPlayer { index } => {
            ability.and_then(|a| a.chosen_players.get(*index as usize).copied())
        }
        // CR 613.1: The player persisted on the source via an "as ~ enters,
        // choose a player" replacement — read durably from the source object.
        ControllerRef::SourceChosenPlayer => {
            crate::game::game_object::source_chosen_player(state, source_id)
        }
        // CR 603.2 + CR 109.4: The player identified by the triggering event.
        ControllerRef::TriggeringPlayer => crate::game::quantity::triggering_event_player(state),
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
    filter_inner(state, object_id, filter, ctx)
}

/// CR 405.1 + CR 115.9b: Match filters against a spell or ability on the
/// stack, including nested "targets ..." predicates on that stack entry.
pub(crate) fn matches_stack_target_filter(
    state: &GameState,
    stack_obj_id: ObjectId,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let Some(entry) = state.stack.iter().find(|entry| entry.id == stack_obj_id) else {
        return false;
    };
    match filter {
        TargetFilter::Any => true,
        TargetFilter::StackSpell => matches!(&entry.kind, StackEntryKind::Spell { .. }),
        TargetFilter::StackAbility {
            controller,
            tag,
            kind,
        } => {
            let ability_kind_ok = kind.as_ref().is_none_or(|kind| {
                matches!(
                    (kind, &entry.kind),
                    (
                        crate::types::ability::StackAbilityKind::Activated,
                        StackEntryKind::ActivatedAbility { .. }
                    ) | (
                        crate::types::ability::StackAbilityKind::Triggered,
                        StackEntryKind::TriggeredAbility { .. }
                    )
                )
            });
            matches!(
                &entry.kind,
                StackEntryKind::ActivatedAbility { .. } | StackEntryKind::TriggeredAbility { .. }
            ) && ability_kind_ok
                && stack_entry_controller_matches(state, controller.as_ref(), entry.controller, ctx)
                // CR 113.7a: keyword-origin tag (e.g. `AbilityTag::Backup`) must
                // match the ability on the stack when the filter requires one.
                && tag.as_ref().is_none_or(|tag| {
                    entry.ability().and_then(|a| a.context.ability_tag.as_ref()) == Some(tag)
                })
        }
        TargetFilter::Typed(tf) => {
            if !tf.type_filters.is_empty() {
                return state.objects.contains_key(&stack_obj_id)
                    && matches_target_filter(state, stack_obj_id, filter, ctx);
            }
            if !stack_entry_controller_matches(state, tf.controller.as_ref(), entry.controller, ctx)
            {
                return false;
            }
            tf.properties.iter().all(|prop| match prop {
                FilterProp::Targets { filter } => stack_entry_targets_satisfy(
                    state,
                    stack_obj_id,
                    ctx.source_id,
                    ctx.source_controller,
                    filter,
                    false,
                ),
                FilterProp::TargetsOnly { filter } => stack_entry_targets_satisfy(
                    state,
                    stack_obj_id,
                    ctx.source_id,
                    ctx.source_controller,
                    filter,
                    true,
                ),
                _ => {
                    state.objects.contains_key(&stack_obj_id)
                        && matches_target_filter(state, stack_obj_id, filter, ctx)
                }
            })
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|inner| matches_stack_target_filter(state, stack_obj_id, inner, ctx)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|inner| matches_stack_target_filter(state, stack_obj_id, inner, ctx)),
        TargetFilter::Not { filter } => {
            !matches_stack_target_filter(state, stack_obj_id, filter, ctx)
        }
        _ => {
            state.objects.contains_key(&stack_obj_id)
                && matches_target_filter(state, stack_obj_id, filter, ctx)
        }
    }
}

fn stack_entry_controller_matches(
    state: &GameState,
    controller: Option<&ControllerRef>,
    entry_controller: PlayerId,
    ctx: &FilterContext<'_>,
) -> bool {
    match controller {
        None => true,
        Some(ControllerRef::You) => ctx.source_controller == Some(entry_controller),
        Some(ControllerRef::Opponent) => ctx
            .source_controller
            .is_some_and(|controller| controller != entry_controller),
        Some(ControllerRef::ScopedPlayer) => scoped_player_or_controller(
            state,
            ctx.ability,
            ctx.source_controller,
            ctx.scoped_iteration_player,
        )
        .is_some_and(|pid| pid == entry_controller),
        Some(ControllerRef::TargetPlayer) => ctx
            .ability
            .and_then(|ability| {
                ability.targets.iter().find_map(|target| match target {
                    TargetRef::Player(pid) => Some(*pid),
                    TargetRef::Object(_) => None,
                })
            })
            .is_some_and(|pid| pid == entry_controller),
        Some(ControllerRef::ParentTargetController) => {
            parent_target_controller_player(state, ctx.ability)
                .is_some_and(|pid| pid == entry_controller)
        }
        Some(ControllerRef::ParentTargetOwner) => parent_target_owner_player(state, ctx.ability)
            .is_some_and(|pid| pid == entry_controller),
        Some(ControllerRef::DefendingPlayer) => {
            crate::game::combat::defending_player_for_attacker(state, ctx.source_id)
                .is_some_and(|pid| pid == entry_controller)
        }
        Some(ControllerRef::SourceChosenPlayer) => {
            crate::game::game_object::source_chosen_player(state, ctx.source_id)
                .is_some_and(|pid| pid == entry_controller)
        }
        Some(ControllerRef::ChosenPlayer { index }) => ctx
            .ability
            .and_then(|ability| ability.chosen_players.get(*index as usize).copied())
            .is_some_and(|pid| pid == entry_controller),
        Some(ControllerRef::TriggeringPlayer) => {
            crate::game::quantity::triggering_event_player(state)
                .is_some_and(|pid| pid == entry_controller)
        }
    }
}

/// CR 702.26b exception: evaluate `filter` against `object_id` **without** the
/// phased-out exclusion that [`matches_target_filter`] applies at its choke
/// point. Phasing-in is one of the rare "rules and effects that specifically
/// mention phased-out permanents," so a mass phase-in must be able to match the
/// very permanents the choke point normally hides. Every other aspect of the
/// filter (controller scope, type, etc.) is evaluated exactly as usual.
pub fn matches_target_filter_including_phased_out(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    filter_inner_for_object(
        state,
        obj,
        object_id,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
        ctx.scoped_iteration_player,
        ControllerLookup::LiveOnly,
    )
}

/// CR 205: Evaluate a `TargetFilter`'s STATIC characteristics against a bare
/// `CardFace` — a printed card definition with no battlefield object, controller,
/// or game-state context (e.g. a card outside the game, or a pool entry hydrated
/// for `Effect::CreateTokenCopyFromPool`). Only context-free predicates are
/// honored (card types, subtypes, supertypes); any filter component that needs a
/// live object (controller scope, counters, combat state, LKI) cannot match a
/// face and yields `false`. Use the object-based `matches_target_filter` family
/// instead whenever an `ObjectId` exists.
pub(crate) fn matches_target_filter_against_face(face: &CardFace, filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::None => false,
        TargetFilter::Typed(typed) => {
            typed.controller.is_none()
                && typed
                    .type_filters
                    .iter()
                    .all(|type_filter| matches_type_filter_against_face(face, type_filter))
                && typed.properties.iter().all(|property| match property {
                    FilterProp::HasSupertype { value } => face.card_type.supertypes.contains(value),
                    _ => false,
                })
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|inner| matches_target_filter_against_face(face, inner)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|inner| matches_target_filter_against_face(face, inner)),
        TargetFilter::Not { filter } => !matches_target_filter_against_face(face, filter),
        _ => false,
    }
}

/// CR 205: Evaluate a single `TypeFilter` against a bare `CardFace`'s printed
/// card type line (core types, subtypes, supertypes). Context-free counterpart
/// to the object-based type checks in `filter_inner_for_object`.
pub(crate) fn matches_type_filter_against_face(face: &CardFace, filter: &TypeFilter) -> bool {
    match filter {
        TypeFilter::Creature => face.card_type.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => face.card_type.core_types.contains(&CoreType::Land),
        TypeFilter::Artifact => face.card_type.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => face.card_type.core_types.contains(&CoreType::Enchantment),
        TypeFilter::Instant => face.card_type.core_types.contains(&CoreType::Instant),
        TypeFilter::Sorcery => face.card_type.core_types.contains(&CoreType::Sorcery),
        TypeFilter::Planeswalker => face.card_type.core_types.contains(&CoreType::Planeswalker),
        TypeFilter::Battle => face.card_type.core_types.contains(&CoreType::Battle),
        // CR 308.1: Kindred card-type check.
        TypeFilter::Kindred => face.card_type.core_types.contains(&CoreType::Kindred),
        TypeFilter::Permanent => face
            .card_type
            .core_types
            .iter()
            .any(|card_type| card_type.is_permanent_type()),
        TypeFilter::Card | TypeFilter::Any => true,
        TypeFilter::Non(inner) => !matches_type_filter_against_face(face, inner),
        TypeFilter::Subtype(subtype) => face.card_type.subtypes.contains(subtype),
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .any(|inner| matches_type_filter_against_face(face, inner)),
    }
}

/// CR 109.5 + CR 400.3: In owner-scoped zones (hand, library, graveyard),
/// Oracle text still says "your card" even though cards are owned rather than
/// controlled there. Evaluate the same typed filter with ownership standing in
/// for controller so control-change LKI on the object cannot exclude its owner.
pub fn matches_target_filter_in_owner_zone(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.is_phased_out() {
        return false;
    }

    // Fast path: when the object is already controlled by its owner — the
    // common case for objects in hand/library/graveyard, where control-change
    // effects almost never apply — the owner-override is a no-op. Skip the
    // `GameObject` clone entirely (it allocates `name`, `counters`, and several
    // Vecs per call, which is hot on library scans for tutors/search effects).
    // Behavior is identical: the override only changes the result when
    // `controller != owner`.
    if obj.controller == obj.owner {
        return filter_inner_for_object(
            state,
            obj,
            object_id,
            filter,
            ctx.source_id,
            ctx.source_controller,
            ctx.ability,
            ctx.recipient_id,
            ctx.scoped_iteration_player,
            ControllerLookup::LiveOnly,
        );
    }

    let mut owner_scoped = obj.clone();
    owner_scoped.controller = owner_scoped.owner;
    filter_inner_for_object(
        state,
        &owner_scoped,
        object_id,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
        ctx.scoped_iteration_player,
        ControllerLookup::LiveOnly,
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
                ctx.scoped_iteration_player,
                ControllerLookup::LiveOrLki,
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
        ctx.scoped_iteration_player,
        ControllerLookup::LiveOrLki,
    )
}

/// CR 508.1a + CR 608.2c: Check whether an attacker declaration snapshot
/// matches a target filter using declaration-time characteristics.
pub fn matches_target_filter_on_attack_declaration_record(
    state: &GameState,
    record: &AttackDeclarationRecord,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    let mut obj = GameObject::new(
        record.object_id,
        CardId(0),
        record.lki.owner,
        record.lki.name.clone(),
        Zone::Battlefield,
    );
    obj.controller = record.lki.controller;
    obj.power = record.lki.power;
    obj.toughness = record.lki.toughness;
    obj.base_power = record.lki.base_power;
    obj.base_toughness = record.lki.base_toughness;
    obj.card_types.core_types = record.lki.card_types.clone();
    obj.card_types.subtypes = record.lki.subtypes.clone();
    obj.card_types.supertypes = record.lki.supertypes.clone();
    obj.mana_cost = ManaCost::generic(record.lki.mana_value);
    obj.keywords = record.lki.keywords.clone();
    obj.color = record.lki.colors.clone();
    obj.counters = record.lki.counters.clone();
    obj.is_token = record.is_token;
    obj.is_commander = record.is_commander;

    filter_inner_for_object(
        state,
        &obj,
        record.object_id,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
        ctx.scoped_iteration_player,
        ControllerLookup::LiveOrLki,
    )
}

/// CR 120.9 + CR 608.2i + CR 608.2h: Check whether a per-turn combat-damage
/// snapshot's *source* matches a target filter using the source's event-time
/// characteristics. Look-back queries ("opponents who were dealt combat damage
/// by ~ or a Dragon this turn", Estinien Varlineau) match against the source as
/// it was when the damage was dealt (CR 608.2i — criteria need not still hold);
/// the source may have since changed type, left the battlefield, or been
/// removed. `SelfRef` matches iff the snapshot's source is the ability source.
pub fn matches_target_filter_on_damage_record_source(
    state: &GameState,
    record: &DamageRecord,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    // CR 608.2i + CR 608.2h: reconstruct the synthetic source with its
    // damage-time zone (Stack for a spell, Battlefield for a permanent) so a
    // zone-discriminating look-back source filter evaluates correctly instead
    // of against an assumed battlefield.
    let mut obj = GameObject::new(
        record.source_id,
        CardId(0),
        record.source_owner,
        record.source_name.clone(),
        record.source_zone,
    );
    obj.controller = record.source_controller_snapshot;
    obj.power = record.source_power;
    obj.toughness = record.source_toughness;
    obj.card_types.core_types = record.source_core_types.clone();
    obj.card_types.subtypes = record.source_subtypes.clone();
    obj.card_types.supertypes = record.source_supertypes.clone();
    obj.mana_cost = crate::types::mana::ManaCost::generic(record.source_mana_value);
    obj.keywords = record.source_keywords.clone();
    obj.color = record.source_colors.clone();

    filter_inner_for_object(
        state,
        &obj,
        record.source_id,
        filter,
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
        ctx.scoped_iteration_player,
        ControllerLookup::LiveOrLki,
    )
}

/// CR 400.7 + CR 608.2h: Evaluate a target filter against last-known information.
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
        trigger_definitions: Vec::new(),
        power: lki.power,
        toughness: lki.toughness,
        // CR 208.4b + CR 613.4b: Carry base P/T into the synthesized record so
        // the base-scope `PtComparison` arm reads the LKI base value.
        base_power: lki.base_power,
        base_toughness: lki.base_toughness,
        colors: lki.colors.clone(),
        mana_value: lki.mana_value,
        controller: lki.controller,
        owner: lki.owner,
        from_zone: None,
        cast_from_zone: None,
        played_from_zone: None,
        to_zone: Zone::Battlefield,
        attachments: vec![],
        linked_exile_snapshot: vec![],
        is_token: false,
        combat_status: Default::default(),
        co_departed: Vec::new(),
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
    ctx: &FilterContext<'_>,
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
        ctx.source_id,
        ctx.source_controller,
        ctx.ability,
        ctx.recipient_id,
        ctx.scoped_iteration_player,
        ControllerLookup::LiveOrLki,
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
    scoped_iteration_player: Option<PlayerId>,
    controller_lookup: ControllerLookup,
) -> bool {
    match filter {
        TargetFilter::None => false,
        TargetFilter::Any => true,
        TargetFilter::Player => false, // Players are not objects
        // CR 118.12a: unless-payer population — never matches an object.
        TargetFilter::AllPlayers => false,
        TargetFilter::Controller => false, // Controller is a player, not an object
        // CR 109.5: OriginalController is a player reference, not an object.
        TargetFilter::OriginalController => false,
        // CR 607.2d + CR 608.2c: SourceChosenPlayer is a player reference, not an object.
        TargetFilter::SourceChosenPlayer => false,
        TargetFilter::ScopedPlayer => false, // ScopedPlayer is a player, not an object
        TargetFilter::SelfRef => object_id == source_id,
        TargetFilter::SourceOrPaired => state
            .objects
            .get(&source_id)
            .and_then(|source| source.paired_with)
            .is_some_and(|paired| object_id == source_id || object_id == paired),
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
            //
            // CR 109.4 + CR 608.2h + CR 400.7: All ControllerRef arms compare
            // against the object's *effective* controller, which falls back to
            // the LKI snapshot only for zones without controllers (Oversimplify class:
            // "creatures they controlled that were exiled this way" must
            // match the at-exile controller, not the post-exile owner). On
            // the stack and battlefield, `effective_controller` returns
            // `obj.controller` unchanged. See the helper for the LKI-fallback
            // rationale.
            if let Some(ctrl) = controller {
                let obj_ctrl = effective_controller(state, obj, object_id, controller_lookup);
                match ctrl {
                    ControllerRef::You => {
                        if source_controller != Some(obj_ctrl) {
                            return false;
                        }
                    }
                    ControllerRef::Opponent => {
                        if source_controller == Some(obj_ctrl) {
                            return false;
                        }
                        // CR 102.3 + CR 800.4a: A player who has left the game is
                        // not an opponent; cards in their zones are not legal
                        // targets (Captain N'ghathrod class).
                        if !super::players::is_alive(state, obj_ctrl) {
                            return false;
                        }
                    }
                    ControllerRef::ScopedPlayer => {
                        match scoped_player_or_controller(
                            state,
                            ability,
                            source_controller,
                            scoped_iteration_player,
                        ) {
                            Some(pid) if pid == obj_ctrl => {}
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
                        let target_player = ability
                            .and_then(|a| {
                                a.targets.iter().find_map(|t| match t {
                                    TargetRef::Player(pid) => Some(*pid),
                                    TargetRef::Object(_) => None,
                                })
                            })
                            // CR 603.2: When no player target was chosen, "that
                            // player" is the triggering event's player. Non-Phase
                            // triggers resolve their player anaphor from event
                            // context, not a chosen/auto-bound target — Hellkite
                            // Tyrant's "all artifacts that player controls" on a
                            // combat-damage trigger. Mirrors the TriggeringPlayer
                            // arm below; inert outside a trigger.
                            .or_else(|| crate::game::quantity::triggering_event_player(state));
                        match target_player {
                            Some(pid) if pid == obj_ctrl => {}
                            _ => return false,
                        }
                    }
                    ControllerRef::ParentTargetController => {
                        let target_player = parent_target_controller_player(state, ability);
                        match target_player {
                            Some(pid) if pid == obj_ctrl => {}
                            _ => return false,
                        }
                    }
                    ControllerRef::ParentTargetOwner => {
                        let target_player = parent_target_owner_player(state, ability);
                        match target_player {
                            Some(pid) if pid == obj_ctrl => {}
                            _ => return false,
                        }
                    }
                    ControllerRef::DefendingPlayer => {
                        match crate::game::combat::defending_player_for_attacker(state, source_id) {
                            Some(pid) if pid == obj_ctrl => {}
                            _ => return false,
                        }
                    }
                    // CR 613.1: "the chosen player controls" — match against the
                    // player persisted on the source.
                    ControllerRef::SourceChosenPlayer => {
                        match crate::game::game_object::source_chosen_player(state, source_id) {
                            Some(pid) if pid == obj_ctrl => {}
                            _ => return false,
                        }
                    }
                    // CR 608.2c + CR 109.4: "a creature they control" following a
                    // `Choose(Player)` — match the object's controller against the
                    // resolution-scoped chosen player.
                    ControllerRef::ChosenPlayer { index } => {
                        match ability.and_then(|a| a.chosen_players.get(*index as usize).copied()) {
                            Some(pid) if pid == obj_ctrl => {}
                            _ => return false,
                        }
                    }
                    // CR 603.2 + CR 109.4: "an opponent who controls F" — match
                    // the object's controller against the triggering player.
                    ControllerRef::TriggeringPlayer => {
                        match crate::game::quantity::triggering_event_player(state) {
                            Some(pid) if pid == obj_ctrl => {}
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
            scoped_iteration_player,
            controller_lookup,
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
                scoped_iteration_player,
                controller_lookup,
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
                scoped_iteration_player,
                controller_lookup,
            )
        }),
        // CR 405.1 + CR 115.9b: stack-target predicates can be composed inside
        // normal object filters, e.g. "spell or ability that targets ...".
        TargetFilter::StackSpell | TargetFilter::StackAbility { .. } => {
            matches_stack_target_filter(
                state,
                object_id,
                filter,
                &FilterContext {
                    source_id,
                    source_controller,
                    ability,
                    recipient_id,
                    scoped_iteration_player,
                },
            )
        }
        TargetFilter::SpecificObject { id: target_id } => object_id == *target_id,
        // SpecificPlayer scopes to players, not objects — no object matches.
        TargetFilter::SpecificPlayer { .. } => false,
        // CR 102.1 + CR 103.1: Neighbor scopes to a seating-relative player,
        // not an object — no object matches.
        TargetFilter::Neighbor { .. } => false,
        TargetFilter::AttachedTo => state
            .objects
            .get(&source_id)
            .and_then(|src| src.attached_to)
            .and_then(|t| t.as_object())
            .is_some_and(|attached| attached == object_id),
        TargetFilter::LastCreated => state.last_created_token_ids.contains(&object_id),
        TargetFilter::LastRevealed => state.last_revealed_ids.contains(&object_id),
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
        // subset matching the inner type. The `id` here is already concrete:
        // the parser emits `TrackedSetId(0)` as a sentinel, but every resolver
        // path binds it to a real set before this match is reached via
        // `targeting::resolve_tracked_set_sentinel`. A still-sentinel `0`
        // therefore matches no objects, which is the correct fallback when no
        // tracked set is available.
        TargetFilter::TrackedSetFiltered {
            id,
            filter,
            caused_by,
        } => {
            // CR 608.2c: `TrackedSetId(0)` is a sentinel for "the most recent
            // tracked set"; resolve it to the concrete set so the `caused_by`
            // check can consult the same set's producer-action provenance.
            let resolved = if id.0 == 0 {
                state
                    .tracked_object_sets
                    .iter()
                    .max_by_key(|(tracked_id, _)| tracked_id.0)
                    .map(|(tracked_id, set)| (*tracked_id, set))
            } else {
                state.tracked_object_sets.get(id).map(|set| (*id, set))
            };
            let in_set = resolved.is_some_and(|(set_id, set)| {
                if !set.contains(&object_id) {
                    return false;
                }
                // CR 608.2c + CR 614.6: an action-bound consumer ("exiled this
                // way", "sacrificed this way", …) matches only members whose
                // recorded producer action equals the bound cause — independent
                // of the member's final zone. `None` keeps the legacy "any
                // member" behavior (selection sets, dig anaphors).
                match caused_by {
                    None => true,
                    Some(cause) => state
                        .tracked_set_member_causes
                        .get(&set_id)
                        .and_then(|causes| causes.get(&object_id))
                        .is_some_and(|member_cause| member_cause == cause),
                }
            });
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
                    scoped_iteration_player,
                    controller_lookup,
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
        // CR 607.2a: References a specific card exiled by the source, indexed by order.
        // Used by The Mimeoplasm to distinguish "the first card exiled this way" from
        // "the second card exiled this way". ENGINE INVARIANT: The ordering is
        // guaranteed by Vec::push in push_exiled_with_source_this_turn.
        TargetFilter::ExiledCardByIndex { index } => {
            // Look up the source's exile list and check if object_id matches the indexed position
            let exiled_cards = state.cards_exiled_with_source_this_turn.get(&source_id);
            exiled_cards
                .and_then(|cards| cards.get(*index as usize))
                .is_some_and(|&card_id| card_id == object_id)
        }
        // CR 603.7c: Event-context references resolve to players, not objects.
        TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::DefendingPlayer => false,
        // ParentTarget/ParentTargetController/ParentTargetOwner/PostReplacementSourceController
        // resolve at resolution time, not via object matching. ParentTargetOwner
        // mirrors ParentTargetController for the player-axis side of CR 108.3 vs CR 109.4.
        TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget => false,
        // CR 201.2 + CR 602.5: "card with the chosen name" — match against source's
        // ChosenAttribute::CardName. The chosen name comes from a player UI prompt;
        // the comparison must mirror the spell-cast prohibition path
        // (`cant_cast_filter_matches`) which uses `eq_ignore_ascii_case`. Without
        // parity, Pithing Needle's activation-prohibition leg would silently miss
        // names that differ only by casing from the player's typed input.
        TargetFilter::HasChosenName => {
            let chosen_name = state.objects.get(&source_id).and_then(|obj| {
                obj.chosen_attributes.iter().find_map(|a| match a {
                    ChosenAttribute::CardName(n) => Some(n.as_str()),
                    _ => None,
                })
            });
            chosen_name.is_some_and(|name| obj.name.eq_ignore_ascii_case(name))
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
    let ch = &spec.characteristics;
    let mut obj = GameObject::new(
        ObjectId(u64::MAX),
        CardId(0),
        owner,
        ch.display_name.clone(),
        Zone::Battlefield,
    );
    obj.controller = owner;
    obj.is_token = true;
    obj.power = ch.power;
    obj.toughness = ch.toughness;
    obj.base_power = ch.power;
    obj.base_toughness = ch.toughness;
    obj.card_types.core_types = ch.core_types.clone();
    obj.card_types.subtypes = ch.subtypes.clone();
    obj.card_types.supertypes = ch.supertypes.clone();
    obj.base_card_types = obj.card_types.clone();
    obj.color = ch.colors.clone();
    obj.base_color = ch.colors.clone();
    obj.keywords = ch.keywords.clone();
    obj.base_keywords = ch.keywords.clone();
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
        // CR 118.12a: unless-payer population — never matches an object.
        TargetFilter::AllPlayers => false,
        TargetFilter::Controller => false,
        // CR 109.5: OriginalController is a player reference, not an object.
        TargetFilter::OriginalController => false,
        // CR 607.2d + CR 608.2c: SourceChosenPlayer is a player reference, not an object.
        TargetFilter::SourceChosenPlayer => false,
        TargetFilter::ScopedPlayer => false,
        TargetFilter::SelfRef => record.object_id == source_id,
        TargetFilter::SourceOrPaired => false,
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
                        match scoped_player_or_controller(state, ability, source_controller, None) {
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
                    ControllerRef::ParentTargetController => {
                        let target_player = parent_target_controller_player(state, ability);
                        match target_player {
                            Some(pid) if pid == record.controller => {}
                            _ => return false,
                        }
                    }
                    // CR 608.2c + CR 109.4: match the spell record's controller
                    // against the resolution-scoped chosen player.
                    ControllerRef::ChosenPlayer { index } => {
                        match ability.and_then(|a| a.chosen_players.get(*index as usize).copied()) {
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
        // CR 102.1 + CR 103.1: Neighbor scopes to a seating-relative player,
        // not an object — a zone-change record is always an object transition.
        TargetFilter::Neighbor { .. } => false,
        // CR 201.2: Zone-change record path mirrors the live-object path —
        // case-insensitive comparison matches the player UI prompt's input.
        TargetFilter::HasChosenName => {
            let chosen_name = state.objects.get(&source_id).and_then(|obj| {
                obj.chosen_attributes.iter().find_map(|a| match a {
                    ChosenAttribute::CardName(n) => Some(n.as_str()),
                    _ => None,
                })
            });
            chosen_name.is_some_and(|name| record.name.eq_ignore_ascii_case(name))
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
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::StackAbility { .. }
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

fn subtype_matches_host_supertype(subtype: &str, supertypes: &[Supertype]) -> bool {
    subtype.eq_ignore_ascii_case("host") && supertypes.contains(&Supertype::Host)
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
        // CR 308.1: Kindred type check.
        TypeFilter::Kindred => obj.card_types.core_types.contains(&CoreType::Kindred),
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
        TypeFilter::Subtype(ref sub) => {
            subtype_matches_with_changeling(
                sub,
                &obj.card_types.subtypes,
                &obj.keywords,
                all_creature_types,
            ) || subtype_matches_host_supertype(sub, &obj.card_types.supertypes)
        }
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
        // CR 308.1: Kindred type check.
        TypeFilter::Kindred => record.core_types.contains(&CoreType::Kindred),
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
        TypeFilter::Subtype(subtype) => {
            subtype_matches_with_changeling(
                subtype,
                &record.subtypes,
                &record.keywords,
                all_creature_types,
            ) || subtype_matches_host_supertype(subtype, &record.supertypes)
        }
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
                    ControllerRef::ParentTargetOwner => return false,
                    ControllerRef::ParentTargetController => return false,
                    ControllerRef::DefendingPlayer => return false,
                    // CR 613.1: "the chosen player" has no meaning for a
                    // spell-history record. Fail closed.
                    ControllerRef::SourceChosenPlayer => return false,
                    // CR 109.4: A chosen-player scope has no meaning for a
                    // spell-history record (no resolution context). Fail closed.
                    ControllerRef::ChosenPlayer { .. } => return false,
                    // CR 603.2 + CR 109.4: A triggering-player scope has no
                    // meaning for a spell-history record (no event context).
                    // Fail closed.
                    ControllerRef::TriggeringPlayer => return false,
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
        // CR 118.12a: unless-payer population, never an object filter.
        | TargetFilter::AllPlayers
        | TargetFilter::Controller
        | TargetFilter::OriginalController
        | TargetFilter::ScopedPlayer
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
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
            // CR 109.1 is cited as the identity foundation here (an object
            // is uniquely the object that it is) because the Comprehensive
            // Rules have no dedicated entry defining "another" — the
            // standard reading across the rules text is "an object distinct
            // from the referenced object". Thread the cast-spell's
            // object_id through so `FilterProp::Another` ("other Dragon
            // spells you cast") can exclude the case where the spell being
            // cast IS the static's own source (e.g. casting The Ur-Dragon
            // itself from the command zone — Eminence must not reduce its
            // own cost).
            spell_object_id: Some(spell_obj.id),
        }),
    )
}

fn spell_cast_record_from_object(spell_obj: &GameObject) -> SpellCastRecord {
    SpellCastRecord {
        name: spell_obj.name.clone(),
        core_types: spell_obj.card_types.core_types.clone(),
        supertypes: spell_obj.card_types.supertypes.clone(),
        subtypes: spell_obj.card_types.subtypes.clone(),
        keywords: spell_obj.keywords.clone(),
        colors: spell_obj.color.clone(),
        // CR 202.3e: While on the stack, X equals the announced value, not 0.
        mana_value: spell_obj
            .mana_cost
            .mana_value_with_x(spell_obj.zone, spell_obj.cost_x_paid),
        has_x_in_cost: crate::game::casting_costs::cost_has_x(&spell_obj.mana_cost),
        from_zone: spell_obj.zone,
        // CR 702.33d: Kicker-paid state for "first kicked spell" cost reducers.
        was_kicked: !spell_obj.kickers_paid.is_empty(),
        cast_variant: crate::types::game_state::CastingVariant::Normal,
    }
}

#[derive(Clone, Copy)]
struct SpellFilterContext<'a> {
    state: &'a GameState,
    source_id: ObjectId,
    source_controller: PlayerId,
    /// CR 109.1 (cited as identity foundation — CR has no dedicated
    /// "another" entry): ObjectId of the spell being filtered. `None` when
    /// the caller is matching against a historical `SpellCastRecord`
    /// (CR 117.x turn-history queries) for which `Another` is structurally
    /// indeterminate — those callers fail-closed on `Another`. Live
    /// cost-modifier evaluation passes `Some(spell.id)` so "other [X]
    /// spells you cast" excludes the static's own source.
    spell_object_id: Option<ObjectId>,
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
                    ControllerRef::ParentTargetController => return false,
                    ControllerRef::DefendingPlayer => return false,
                    // CR 109.4: Chosen-player scope is undefined for spell-cast
                    // history (no resolution context). Fail closed.
                    ControllerRef::ChosenPlayer { .. } => return false,
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
        // CR 118.12a: unless-payer population, never an object filter.
        | TargetFilter::AllPlayers
        | TargetFilter::Controller
        | TargetFilter::OriginalController
        | TargetFilter::ScopedPlayer
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringSourceController
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
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
        FilterProp::ManaValueParity { parity } => {
            let choice = context.and_then(|ctx| ctx.state.last_named_choice.as_ref());
            mana_value_matches_parity_source(record.mana_value, parity, choice)
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
        FilterProp::MostPrevalentCreatureTypeIn { .. } => false,
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
            // CR 205.2a: `chosen_card_type()` resolves both the `CardType`
            // attribute and a restricted card-type `Label` ("Choose creature or
            // land", Winding Way) to a `CoreType`, so this matcher binds both
            // the generic "choose a card type" and the labeled forms uniformly.
            context
                .state
                .objects
                .get(&context.source_id)
                .and_then(|source| source.chosen_card_type())
                .is_some_and(|card_type| record.core_types.contains(&card_type))
        }),
        // CR 109.1 (cited as identity foundation — CR has no dedicated
        // "another" entry): "other [X] spells you cast" excludes the case
        // where the spell being cast IS the static's own source object. The
        // check is identity-only (`object_id != source_id`); two distinct
        // copies of the same card are NOT "the same" object. Historical-
        // record callers pass `spell_object_id: None` and fail-closed here
        // (a turn-history "another" query needs the original cast's
        // object_id, which is not stored in the snapshot — CR 117.x
        // predicates that need it must route through dedicated `Another`-
        // aware paths).
        FilterProp::Another => context.is_some_and(|ctx| {
            ctx.spell_object_id
                .is_some_and(|spell_id| spell_id != ctx.source_id)
        }),
        // CR 601.2f: Spell cost modifiers may require the spell share a quality
        // (e.g., card type) with a reference object such as an imprinted card.
        FilterProp::SharesQuality {
            quality,
            reference,
            relation,
        } => {
            let Some(context) = context else {
                return false;
            };
            let Some(spell_id) = context.spell_object_id else {
                return false;
            };
            let Some(spell_obj) = context.state.objects.get(&spell_id) else {
                return false;
            };
            let source = source_context_from_spell_filter(context);
            evaluate_shares_quality(
                context.state,
                spell_obj,
                quality,
                reference,
                relation,
                &source,
            )
        }
        // CR 702.33d: Kicker-paid during live cost-modifier evaluation.
        FilterProp::WasKicked => context.is_some_and(|ctx| {
            let Some(spell_id) = ctx.spell_object_id else {
                return false;
            };
            if let Some(pending) = ctx.state.pending_cast.as_ref() {
                if pending.object_id == spell_id {
                    return !pending.ability.context.kickers_paid.is_empty();
                }
            }
            ctx.state
                .objects
                .get(&spell_id)
                .is_some_and(|obj| !obj.kickers_paid.is_empty())
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
        // CR 308.1: Kindred type check.
        TypeFilter::Kindred => record.core_types.contains(&CoreType::Kindred),
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
        TypeFilter::Subtype(subtype) => {
            subtype_matches_with_changeling(
                subtype,
                &record.subtypes,
                &record.keywords,
                all_creature_types,
            ) || subtype_matches_host_supertype(subtype, &record.supertypes)
        }
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
        // CR 303.4: "could enchant [target]" needs live target context and
        // Aura attachment legality; stack snapshots only record keyword values.
        FilterProp::CanEnchant { .. } => false,
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
        FilterProp::NotHistoric => !spell_record_matches_property(record, &FilterProp::Historic),
        FilterProp::ColorCount { comparator, count } => {
            comparator.evaluate(record.colors.len() as i32, i32::from(*count))
        }
        FilterProp::Cmc { comparator, value } => match value {
            QuantityExpr::Fixed { value: v } => comparator.evaluate(record.mana_value as i32, *v),
            _ => {
                debug_assert!(false, "dynamic QuantityExpr in spell record Cmc filter — parser should only produce Fixed values here");
                false
            }
        },
        FilterProp::ManaValueParity { parity } => {
            mana_value_matches_parity_source(record.mana_value, parity, None)
        }
        // CR 202.1: Exact printed mana cost is not captured in cast-history
        // snapshots. Fail closed rather than approximating with mana value
        // (CR 202.3), which would conflate {W} with {1}.
        FilterProp::ManaCostIn { .. } => false,
        // CR 107.3 + CR 202.1: The snapshot captured whether the printed mana
        // cost contained an `{X}` shard at cast time.
        FilterProp::HasXInManaCost => record.has_x_in_cost,
        FilterProp::WasKicked => record.was_kicked,
        FilterProp::HasXInActivationCost => false,
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
        // CR 608.2c: Logical negation — recurse under the same snapshot and invert.
        FilterProp::Not { prop } => !spell_record_matches_property(record, prop),
        // CR 111.1: Spell-cast records only track cast spells. Tokens are
        // permanents, so token identity is false and nontoken identity is true
        // for this snapshot shape.
        FilterProp::Token => false,
        FilterProp::NonToken => true,
        FilterProp::WasPlayed => true,
        FilterProp::InZone { zone: required } => record.from_zone == *required,
        // CR 400.1 + CR 601.2a: cast-origin membership — the record's captured
        // from_zone (populated when the spell was put on the stack from where it
        // was, CR 601.2a) is one of the listed cast-capable zones. Mirrors the
        // InZone arm; used by "spell you've cast this turn from anywhere other
        // than your hand" (the Paradox cycle).
        FilterProp::InAnyZone { zones } => zones.contains(&record.from_zone),
        // CR 201.2: Exact name match against the cast-time snapshot — case-
        // insensitive per the same convention used by the live-object path.
        // Approach of the Second Sun's "you've cast another spell named
        // {LITERAL} this game" relies on this against the game-scope history.
        FilterProp::Named { name } => record.name.eq_ignore_ascii_case(name),
        // All remaining props require on-battlefield or stack state unavailable from a snapshot.
        FilterProp::Attacking { .. }
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::Untapped
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::Counters { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::PtComparison { .. }
        | FilterProp::PowerGTSource
        | FilterProp::IsChosenCreatureType
        | FilterProp::MostPrevalentCreatureTypeIn { .. }
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::HasSingleTarget
        | FilterProp::Suspected
        | FilterProp::Renowned
        // CR 700.9: Modified requires on-battlefield attachments/counters,
        // unavailable from a stack-snapshot record.
        | FilterProp::Modified
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::DifferentNameFrom { .. }
        | FilterProp::SharesQuality { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        // CR 122.6: A spell on the stack hasn't received counters as a
        // permanent — fail closed against the spell-cast snapshot.
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        // CR 201.2: Source-/target-relative name predicates require
        // resolution context the spell-history scan doesn't currently plumb
        // — fail closed until a card forces that plumbing.
        // `FilterProp::Named { name }` is handled above against the snapshot.
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
    let id = first_object_target(ability)?;
    if let Some(obj) = state.objects.get(&id) {
        return Some(obj.name.clone());
    }
    state.lki_cache.get(&id).map(|lki| lki.name.clone())
}

fn first_object_target(ability: &ResolvedAbility) -> Option<ObjectId> {
    ability.targets.iter().find_map(|target| match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    })
}

fn combat_relation_subject_id(
    subject: CombatRelationSubject,
    source: &SourceContext<'_>,
) -> Option<ObjectId> {
    match subject {
        CombatRelationSubject::Source => Some(source.id),
        CombatRelationSubject::ParentTarget => source.ability.and_then(first_object_target),
    }
}

fn matches_combat_relation(
    state: &GameState,
    object_id: ObjectId,
    relation: CombatRelation,
    subject: CombatRelationSubject,
    source: &SourceContext<'_>,
) -> bool {
    let Some(subject_id) = combat_relation_subject_id(subject, source) else {
        return false;
    };
    match relation {
        CombatRelation::BlockingOrBlockedBy => state.combat.as_ref().is_some_and(|combat| {
            let candidate_blocks_subject = combat
                .blocker_to_attacker
                .get(&object_id)
                .is_some_and(|attackers| attackers.contains(&subject_id));
            let subject_blocks_candidate = combat
                .blocker_to_attacker
                .get(&subject_id)
                .is_some_and(|attackers| attackers.contains(&object_id));
            candidate_blocks_subject || subject_blocks_candidate
        }),
    }
}

fn referenced_targets_for_filter<'a>(
    target: &TargetFilter,
    ability: Option<&'a ResolvedAbility>,
) -> Vec<&'a TargetRef> {
    let Some(ability) = ability else {
        return vec![];
    };
    match target {
        // Returns the chosen object targets only. For an *untargeted* `ParentTarget`
        // referent (e.g. a permanent the parent `Sacrifice` effect chose while
        // applying — CR 608.2k), there is no `TargetRef` here, and synthesizing a
        // fake one would lie to aura-enchant and object-list consumers since the
        // object no longer exists. The resolution route for an untargeted
        // `ParentTarget` referent is the effect-context LKI snapshot consulted by
        // `parent_target_shared_quality_values`, not this list — the empty arm is
        // intentional, not a gap.
        TargetFilter::ParentTarget => ability.targets.iter().collect(),
        TargetFilter::ParentTargetSlot { index } => {
            ability.targets.get(*index).into_iter().collect()
        }
        _ => vec![],
    }
}

fn aura_can_enchant_referenced_target(
    state: &GameState,
    aura: &GameObject,
    aura_id: ObjectId,
    enchant_filter: &TargetFilter,
    target_ref: &TargetRef,
    source: &SourceContext<'_>,
) -> bool {
    match target_ref {
        TargetRef::Object(target_id) => {
            let ctx = FilterContext {
                source_id: aura_id,
                source_controller: Some(aura.controller),
                ability: source.ability,
                recipient_id: source.recipient_id,
                scoped_iteration_player: None,
            };
            filter_inner(state, *target_id, enchant_filter, &ctx)
        }
        TargetRef::Player(player_id) => player_matches_target_filter_in_state(
            state,
            enchant_filter,
            *player_id,
            Some(aura.controller),
        ),
    }
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

fn pt_value_from_pair(stat: PtStat, power: Option<i32>, toughness: Option<i32>) -> i32 {
    match stat {
        PtStat::Power => power.unwrap_or(0),
        PtStat::Toughness => toughness.unwrap_or(0),
        PtStat::TotalPowerToughness => power.unwrap_or(0) + toughness.unwrap_or(0),
    }
}

fn object_pt_value(obj: &GameObject, stat: PtStat, scope: PtValueScope) -> i32 {
    match scope {
        PtValueScope::Current => pt_value_from_pair(stat, obj.power, obj.toughness),
        PtValueScope::Base => pt_value_from_pair(stat, obj.base_power, obj.base_toughness),
    }
}

fn zone_change_pt_value(record: &ZoneChangeRecord, stat: PtStat, scope: PtValueScope) -> i32 {
    match scope {
        PtValueScope::Current => pt_value_from_pair(stat, record.power, record.toughness),
        PtValueScope::Base => pt_value_from_pair(stat, record.base_power, record.base_toughness),
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

fn parity_from_source(source: &ParitySource, choice: Option<&ChoiceValue>) -> Option<Parity> {
    match source {
        ParitySource::Fixed(parity) => Some(*parity),
        ParitySource::LastNamedChoice => match choice {
            Some(ChoiceValue::OddOrEven(parity)) => Some(*parity),
            _ => None,
        },
    }
}

fn mana_value_matches_parity(mana_value: u32, parity: Parity) -> bool {
    match parity {
        Parity::Odd => !mana_value.is_multiple_of(2),
        Parity::Even => mana_value.is_multiple_of(2),
    }
}

fn mana_value_matches_parity_source(
    mana_value: u32,
    source: &ParitySource,
    choice: Option<&ChoiceValue>,
) -> bool {
    parity_from_source(source, choice)
        .is_some_and(|parity| mana_value_matches_parity(mana_value, parity))
}

fn attacking_defender_matches(
    state: &GameState,
    source: &SourceContext<'_>,
    defending_player: PlayerId,
    defender: Option<&ControllerRef>,
) -> bool {
    match defender {
        None => true,
        Some(ControllerRef::Opponent) => source.controller.is_some_and(|controller| {
            super::players::is_opponent(state, controller, defending_player)
        }),
        Some(controller) => controller_ref_player(
            state,
            source.id,
            source.controller,
            source.ability,
            controller,
        )
        .is_some_and(|player| player == defending_player),
    }
}

/// Check if an object satisfies a single FilterProp.
/// CR 122.6 + CR 109.5: Match a counter-placement record's actor against a
/// `CountScope` relative to the reference player (the static's controller). This
/// is the filter-evaluation analog of `quantity::count_scope_actor_matches`,
/// which is unavailable here because filter evaluation carries no
/// `QuantityContext` iteration scope. `ScopedPlayer`/`SourceChosenPlayer` fall
/// back to the controller, matching the quantity path's out-of-iteration default.
fn filter_count_scope_actor_matches(
    scope: &CountScope,
    controller: PlayerId,
    actor: PlayerId,
) -> bool {
    match scope {
        CountScope::Controller
        | CountScope::Owner
        | CountScope::ScopedPlayer
        | CountScope::SourceChosenPlayer => actor == controller,
        CountScope::All => true,
        CountScope::Opponents => actor != controller,
    }
}

fn matches_filter_prop(
    prop: &FilterProp,
    state: &GameState,
    obj: &GameObject,
    object_id: ObjectId,
    source: &SourceContext<'_>,
) -> bool {
    match prop {
        // CR 111.1: Token identity of the matched object or event-time snapshot.
        FilterProp::Token => obj.is_token,
        // CR 111.1: Nontoken identity of the matched object or event-time snapshot.
        FilterProp::NonToken => !obj.is_token,
        // CR 305.1 + CR 601.2a: "played by" entry replacements (Uphill Battle).
        FilterProp::WasPlayed => obj.played_from_zone.is_some() || obj.cast_from_zone.is_some(),
        // CR 508.1b: Attacking creatures may be scoped by defending player
        // relation ("attacking", "attacking you", "attacking your opponents").
        FilterProp::Attacking { defender } => state.combat.as_ref().is_some_and(|combat| {
            combat.attackers.iter().any(|a| {
                a.object_id == object_id
                    && attacking_defender_matches(
                        state,
                        source,
                        a.defending_player,
                        defender.as_ref(),
                    )
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
        FilterProp::CombatRelation { relation, subject } => {
            matches_combat_relation(state, object_id, *relation, *subject, source)
        }
        // CR 509.1h: Unblocked = attacking creature that was never assigned blockers.
        // unblocked_attackers checks the permanent `blocked` flag, not the current blocker list.
        FilterProp::Unblocked => combat::unblocked_attackers(state).contains(&object_id),
        // CR 506.5: sole attacker / sole blocker against live combat. Look-back
        // callers route through the zone-change snapshot arm instead.
        FilterProp::AttackingAlone => combat::attacking_alone(state, object_id),
        FilterProp::BlockingAlone => combat::blocking_alone(state, object_id),
        FilterProp::Tapped => obj.tapped,
        // CR 702.171b: Matches permanents with the saddled designation.
        FilterProp::IsSaddled => obj.is_saddled,
        // CR 702.171c: Matches a creature that saddled the filter source this turn
        // (tapped to pay the source's saddle cost — recorded in `saddled_by`,
        // cleared at end of turn). Source-relative, mirroring `BlockingSource`.
        FilterProp::SaddledSource => state
            .objects
            .get(&source.id)
            .is_some_and(|src| src.saddled_by.contains(&object_id)),
        // CR 310.8a: "each battle they protect" — protector is an opponent of
        // the source controller (Joyful Stormsculptor class).
        FilterProp::ProtectorMatches { controller } => {
            if !obj.card_types.core_types.contains(&CoreType::Battle) {
                return false;
            }
            let Some(protector) = obj.protector() else {
                return false;
            };
            match controller {
                ControllerRef::Opponent => source.controller.is_some_and(|sc| sc != protector),
                ControllerRef::You => source.controller == Some(protector),
                _ => false,
            }
        }
        // CR 302.6 / CR 110.5: Untapped status as targeting qualifier.
        FilterProp::Untapped => !obj.tapped,
        // CR 302.6 + CR 702.10b + CR 702.154a: Enlist may tap a creature only
        // if it has haste or has been controlled continuously since turn began.
        FilterProp::HasHasteOrControlledSinceTurnBegan => {
            obj.card_types.core_types.contains(&CoreType::Creature)
                && !combat::has_summoning_sickness(obj)
        }
        FilterProp::WithKeyword { value } => obj.has_keyword(value),
        FilterProp::CanEnchant { target } => obj.keywords.iter().any(|keyword| {
            let Keyword::Enchant(enchant_filter) = keyword else {
                return false;
            };
            referenced_targets_for_filter(target, source.ability)
                .iter()
                .any(|target_ref| {
                    aura_can_enchant_referenced_target(
                        state,
                        obj,
                        object_id,
                        enchant_filter,
                        target_ref,
                        source,
                    )
                })
        }),
        FilterProp::HasKeywordKind { value } => {
            crate::game::keywords::object_has_effective_keyword_kind(state, object_id, *value)
        }
        // CR 702: "without [keyword]" — negated keyword filter.
        FilterProp::WithoutKeyword { value } => !obj.has_keyword(value),
        FilterProp::WithoutKeywordKind { value } => {
            !crate::game::keywords::object_has_effective_keyword_kind(state, object_id, *value)
        }
        // CR 122.1: Counter count threshold over `counters` (a specific type or
        // any type, summed). Dynamic thresholds (`QuantityRef::Variable { "X" }`)
        // resolve against the ability's `chosen_x` when a `ResolvedAbility` is in
        // scope via `FilterContext::from_ability`.
        FilterProp::Counters {
            counters,
            comparator,
            count,
        } => {
            let selector = match counters {
                CounterMatch::Any => None,
                CounterMatch::OfType(ct) => Some(ct),
            };
            let actual = counter_count_from_map(&obj.counters, selector);
            comparator.evaluate(actual, resolve_filter_threshold(state, count, source))
        }
        // CR 202.3: Mana value threshold comparisons. Dynamic thresholds
        // (`QuantityRef::Variable { "X" }`) resolve against the ability's
        // `chosen_x` when a `ResolvedAbility` is in scope via `FilterContext::from_ability`.
        // CR 202.3e: For on-stack objects, X equals the announced value, not 0.
        FilterProp::Cmc { comparator, value } => {
            let cmc = obj.mana_cost.mana_value_with_x(obj.zone, obj.cost_x_paid) as i32;
            comparator.evaluate(cmc, resolve_filter_threshold(state, value, source))
        }
        FilterProp::ManaValueParity { parity } => {
            let mana_value = obj.mana_cost.mana_value_with_x(obj.zone, obj.cost_x_paid);
            mana_value_matches_parity_source(mana_value, parity, state.last_named_choice.as_ref())
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
        // CR 107.3 + CR 601.2f: "{X} in its activation cost" — consult the
        // pending activation record for the matched source object. Composes with
        // typed type/controller filters via `TargetFilter::And`/`Or`.
        FilterProp::HasXInActivationCost => {
            crate::game::casting_costs::pending_activation_cost_has_x(state, object_id)
        }
        FilterProp::WasKicked => {
            if let Some(pending) = state.pending_cast.as_ref() {
                if pending.object_id == object_id {
                    return !pending.ability.context.kickers_paid.is_empty();
                }
            }
            !obj.kickers_paid.is_empty()
        }
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
            // CR 730.2: iterate `state.battlefield` — the authoritative list of
            // INDEPENDENT permanents — so an absorbed merge component (zone is
            // Battlefield but it is not a member of this list) is never counted
            // as a separate permanent. This also avoids an O(n) per-object
            // absorbed-component scan over `state.objects`.
            state.battlefield.iter().any(|perm_id| {
                let Some(perm) = state.objects.get(perm_id) else {
                    return false;
                };
                let controller_ok = match (controller, controller_pid) {
                    (Some(ControllerRef::You), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::Opponent), _) => {
                        source.controller.is_some() && Some(perm.controller) != source.controller
                    }
                    (Some(ControllerRef::ScopedPlayer), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::TargetPlayer), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::ParentTargetController), Some(pid)) => {
                        perm.controller == pid
                    }
                    (Some(ControllerRef::ParentTargetOwner), Some(pid)) => perm.owner == pid,
                    (Some(ControllerRef::DefendingPlayer), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::SourceChosenPlayer), Some(pid)) => perm.controller == pid,
                    (Some(ControllerRef::ChosenPlayer { .. }), Some(pid)) => perm.controller == pid,
                    // CR 603.2 + CR 109.4: triggering-player-scoped name match.
                    (Some(ControllerRef::TriggeringPlayer), Some(pid)) => perm.controller == pid,
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
                source.controller.is_some()
                    && source.controller != Some(obj.owner)
                    && super::players::is_alive(state, obj.owner)
            }
            ControllerRef::ScopedPlayer => {
                scoped_player_or_controller(state, source.ability, source.controller, None)
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
            ControllerRef::ParentTargetController => {
                parent_target_controller_player(state, source.ability)
                    .is_some_and(|pid| pid == obj.owner)
            }
            ControllerRef::ParentTargetOwner => parent_target_owner_player(state, source.ability)
                .is_some_and(|pid| pid == obj.owner),
            ControllerRef::DefendingPlayer => {
                crate::game::combat::defending_player_for_attacker(state, source.id)
                    .is_some_and(|pid| pid == obj.owner)
            }
            // CR 613.1: Ownership relative to the source's persisted chosen player.
            ControllerRef::SourceChosenPlayer => {
                crate::game::game_object::source_chosen_player(state, source.id)
                    .is_some_and(|pid| pid == obj.owner)
            }
            // CR 608.2c + CR 109.4: Ownership relative to a resolution-chosen player.
            ControllerRef::ChosenPlayer { index } => source
                .ability
                .and_then(|a| a.chosen_players.get(*index as usize).copied())
                .is_some_and(|pid| pid == obj.owner),
            // CR 603.2 + CR 109.4: Ownership relative to the triggering player.
            ControllerRef::TriggeringPlayer => {
                crate::game::quantity::triggering_event_player(state)
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
        // CR 303.4 + CR 301.5: Attachment predicate. Matches objects that have
        // at least one attachment of the given kind whose controller satisfies
        // the optional `ControllerRef`. `exclude_source` preserves "another
        // Aura/Equipment" legality after the source becomes attached.
        FilterProp::HasAttachment {
            kind,
            controller,
            exclude_source,
        } => obj.attachments.iter().any(|att_id| {
            if exclude_source.is_exclude() && *att_id == source.id {
                return false;
            }
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
        // CR 702.95b: An unpaired creature is one that is not paired.
        FilterProp::Unpaired => obj.paired_with.is_none(),
        // CR 603.4 + CR 109.3: `OtherThanTriggerObject` is a typed marker that
        // signals "exclude the triggering object" for count semantics. The
        // exclusion is applied at the `QuantityRef::ObjectCount` resolver level
        // (see `game::quantity`) using the current trigger event, not here —
        // this variant acts as a transparent pass-through for per-object
        // filter evaluation so that the marker does not spuriously exclude
        // every object from individual match checks.
        FilterProp::OtherThanTriggerObject => true,
        FilterProp::HasColor { color } => obj.color.contains(color),
        // CR 208 + CR 208.4b: Power/toughness metric comparison against a
        // dynamic threshold. `scope = Base` reads `base_power`/`base_toughness`
        // (the value after layer 7b, ignoring counters/modifiers per
        // CR 208.4b); `scope = Current` reads the fully-modified
        // `power`/`toughness`.
        // Dynamic thresholds (`QuantityRef::Variable { "X" }`) resolve against
        // the ability's `chosen_x` via `FilterContext::from_ability`.
        FilterProp::PtComparison {
            stat,
            scope,
            comparator,
            value,
        } => comparator.evaluate(
            object_pt_value(obj, *stat, *scope),
            resolve_filter_threshold(state, value, source),
        ),
        // Disjunctive composite: any inner prop matches.
        FilterProp::AnyOf { props } => props
            .iter()
            .any(|p| matches_filter_prop(p, state, obj, object_id, source)),
        // CR 608.2c: Logical negation — the object matches iff the inner prop does NOT.
        FilterProp::Not { prop } => !matches_filter_prop(prop, state, obj, object_id, source),
        // CR 509.1b: Object's power is strictly greater than the source object's power.
        FilterProp::PowerGTSource => {
            let source_power = state
                .objects
                .get(&source.id)
                .and_then(|o| o.power)
                .unwrap_or(0);
            obj.power.unwrap_or(0) > source_power
        }
        FilterProp::ColorCount { comparator, count } => {
            comparator.evaluate(obj.color.len() as i32, i32::from(*count))
        }
        FilterProp::HasSupertype { value } => obj.card_types.supertypes.contains(value),
        // CR 205.4b: Object does NOT have this color.
        FilterProp::NotColor { color } => !obj.color.contains(color),
        // CR 205.4a: Object does NOT have this supertype.
        FilterProp::NotSupertype { value } => !obj.card_types.supertypes.contains(value),
        // CR 205.3e + CR 205.3m + CR 702.73a: A chosen creature type matches
        // any listed subtype, and changeling objects have every creature type.
        FilterProp::IsChosenCreatureType => match source.chosen_creature_type {
            Some(chosen) => subtype_matches_with_changeling(
                chosen,
                &obj.card_types.subtypes,
                &obj.keywords,
                &state.all_creature_types,
            ),
            None => false,
        },
        // CR 205.3m: Object's creature type ties for highest count
        // among creature cards in the named player's named zone. Scope picks
        // the player whose zone is inspected; `Opponent` falls back to the
        // candidate object's owner (search-context invariant — the candidate
        // already lives in the inspected zone, so its owner IS that player).
        FilterProp::MostPrevalentCreatureTypeIn { zone, scope } => {
            let owner =
                controller_ref_player(state, source.id, source.controller, source.ability, scope)
                    .unwrap_or(obj.owner);
            most_prevalent_creature_types_in_zone(state, owner, *zone)
                .into_iter()
                .any(|creature_type| {
                    subtype_matches_with_changeling(
                        &creature_type,
                        &obj.card_types.subtypes,
                        &obj.keywords,
                        &state.all_creature_types,
                    )
                })
        }
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
        // CR 205 + CR 205.2a: Match objects whose core type includes the
        // source's chosen card type. Used for "spells of the chosen type"
        // (Archon of Valor's Reach) and "all cards of the chosen type revealed
        // this way" (Winding Way). The chosen type may be persisted as a
        // `CardType` attribute (generic "choose a card type") or, for a
        // restricted card-type choice ("Choose creature or land"), as a
        // capitalized `Label` that names a card type — `chosen_card_type_of`
        // resolves both forms to a `CoreType`.
        FilterProp::IsChosenCardType => {
            crate::game::game_object::chosen_card_type_of(source.chosen_attributes)
                .is_some_and(|chosen| obj.card_types.core_types.contains(&chosen))
        }
        FilterProp::IsChosenLandOrNonlandKind => matches_last_chosen_land_or_nonland_kind(
            &state.last_named_choice,
            &obj.card_types.core_types,
        ),
        // CR 701.60b: Match creatures with the suspected designation.
        FilterProp::Suspected => obj.is_suspected,
        // CR 702.112b: Match permanents with the renowned designation.
        FilterProp::Renowned => obj.is_renowned,
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
        FilterProp::NotHistoric => {
            !matches_filter_prop(&FilterProp::Historic, state, obj, object_id, source)
        }
        // CR 510.1c: Match creatures whose toughness exceeds their power.
        FilterProp::ToughnessGTPower => {
            let power = obj.power.unwrap_or(0);
            let toughness = obj.toughness.unwrap_or(0);
            toughness > power
        }
        // CR 208.1 + CR 613.4b: Match creatures whose current (post-layer) power
        // exceeds their base power (layer-7b baseline incl. CDA, before
        // counters/pumps in 7c–7e).
        FilterProp::PowerExceedsBase => obj.power.unwrap_or(0) > obj.base_power.unwrap_or(0),
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
        } => evaluate_shares_quality(state, obj, quality, reference, relation, source),
        // CR 120.6 + CR 120.9: "Was dealt damage this turn" is a historical fact,
        // not a query against current marked damage. CR 120.6 removes marked damage
        // when a permanent regenerates and during the cleanup step, so reading
        // `damage_marked` would silently lose the fact for any creature that had
        // regenerated. The damage-event history (CR 120.9 establishes "dealt damage"
        // as the per-source historical record) is the authoritative source.
        FilterProp::WasDealtDamageThisTurn => state
            .damage_dealt_this_turn
            .iter()
            .any(|record| matches!(record.target, TargetRef::Object(id) if id == object_id)),
        // CR 400.7: Object entered the battlefield this turn.
        FilterProp::EnteredThisTurn => obj.entered_battlefield_turn == Some(state.turn_number),
        FilterProp::ZoneChangedThisTurn { from, to } => {
            state.zone_changes_this_turn.iter().any(|record| {
                record.object_id == object_id
                    && from.is_none_or(|zone| record.from_zone == Some(zone))
                    && to.is_none_or(|zone| record.to_zone == zone)
            })
        }
        // CR 508.1a: Creature was declared as an attacker this turn.
        FilterProp::AttackedThisTurn => state.creatures_attacked_this_turn.contains(&object_id),
        // CR 509.1a: Creature was declared as a blocker this turn.
        FilterProp::BlockedThisTurn => state.creatures_blocked_this_turn.contains(&object_id),
        // CR 508.1a + CR 509.1a: Creature attacked or blocked this turn.
        FilterProp::AttackedOrBlockedThisTurn => {
            state.creatures_attacked_this_turn.contains(&object_id)
                || state.creatures_blocked_this_turn.contains(&object_id)
        }
        // CR 122.1 + CR 122.6: Object received counters (matching `counters`) from
        // `actor` this turn, summed across all qualifying placement records and
        // tested against `comparator`/`count`. Look-back per CR 122.6 ("counters
        // being put on an object") — a historical-action predicate, so the match
        // survives later removal of those counters. The static's controller
        // (`source.controller`) is the reference player for the `actor` scope.
        FilterProp::CountersPutOnThisTurn {
            actor,
            counters,
            comparator,
            count,
        } => {
            let controller = source.controller.unwrap_or(PlayerId(0));
            let total: u32 = state
                .counter_added_this_turn
                .iter()
                .filter(|record| {
                    record.object_id == object_id
                        && counters.matches(&record.counter_type)
                        && filter_count_scope_actor_matches(actor, controller, record.actor)
                })
                .fold(0, |sum: u32, record| sum.saturating_add(record.count));
            comparator.evaluate(i32::try_from(total).unwrap_or(i32::MAX), *count as i32)
        }
        // CR 115.7: Stack entry has exactly one target — permissive at filter level,
        // validated by retarget effects at resolution time.
        FilterProp::HasSingleTarget => true,
        // CR 115.9c: Stack entry's targets all match the inner filter — permissive at
        // per-object level, validated by trigger matchers and retarget effects against the
        // stack entry's actual targets.
        // CR 707.2: Match face-down permanents on the battlefield.
        FilterProp::FaceDown => obj.face_down,
        // CR 115.9c: If the object is a stack entry, ALL of its targets must match
        // the inner filter. Falls back permissive for non-stack objects so trigger
        // matchers remain the primary authority (they validate separately).
        FilterProp::TargetsOnly { filter } => stack_entry_targets_satisfy(
            state,
            object_id,
            source.id,
            source.controller,
            filter,
            true,
        ),
        // CR 115.9b: Stack entry has at least one target matching the inner filter.
        FilterProp::Targets { filter } => stack_entry_targets_satisfy(
            state,
            object_id,
            source.id,
            source.controller,
            filter,
            false,
        ),
        // CR 903.3d: "If an effect refers to controlling a commander, it refers
        // to a permanent on the battlefield that is a commander." `is_commander`
        // is the deck-construction designation per CR 903.3.
        FilterProp::IsCommander => obj.is_commander,
        FilterProp::Other { .. } => false, // Fail-closed for unrecognized properties
    }
}

/// CR 115.9b/115.9c: Check whether a stack entry's targets satisfy a filter.
///
/// Used by `FilterProp::Targets` and `FilterProp::TargetsOnly` in
/// `matches_filter_prop` when the object being evaluated is a stack entry.
/// If `require_all` is `true` (TargetsOnly / CR 115.9c), every target must
/// match `filter`; if `false` (Targets / CR 115.9b), at least one must.
///
/// Non-stack objects return `true` (permissive fallback) so trigger matchers,
/// which validate targets through `stack_entry_targets_any`, remain the primary
/// authority. Stack entries with no ability targets return `false` because
/// "targets X" cannot be satisfied with an empty target list.
fn stack_entry_targets_satisfy(
    state: &GameState,
    stack_obj_id: ObjectId,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
    filter: &TargetFilter,
    require_all: bool,
) -> bool {
    let Some(entry) = state.stack.iter().find(|e| e.id == stack_obj_id) else {
        return true; // Not a stack entry — permissive.
    };
    let Some(ability) = entry.ability() else {
        return true; // KeywordAction entries carry no ability targets — permissive.
    };
    if ability.targets.is_empty() {
        return false; // "targets X" with no targets cannot be satisfied.
    }
    let ctx = match source_controller {
        Some(controller) => FilterContext::from_source_with_controller(source_id, controller),
        None => FilterContext::from_source(state, source_id),
    };
    let check = |t: &TargetRef| match t {
        TargetRef::Object(id) => matches_target_filter(state, *id, filter, &ctx),
        TargetRef::Player(pid) => {
            player_matches_target_filter_in_state(state, filter, *pid, ctx.source_controller)
        }
    };
    if require_all {
        ability.targets.iter().all(check)
    } else {
        ability.targets.iter().any(check)
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
        // CR 303.4: Requires live target context; zone-change snapshots cannot
        // prove attachment legality against a referenced target.
        FilterProp::CanEnchant { .. } => false,
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
        FilterProp::NotHistoric => {
            !zone_change_record_matches_property(&FilterProp::Historic, state, record, source)
        }
        // CR 201.2: Name match (case-insensitive) on the event-time object.
        FilterProp::Named { name } => record.name.eq_ignore_ascii_case(name),
        // CR 208 + CR 208.4b: Power/toughness metric threshold on the
        // event-time object. A `None` value (non-creature in some zones) treats
        // as 0, matching live-state behavior. The zone-change snapshot captures
        // both the current (post-layer-7) and base (layer-7b, per CR 613.4b)
        // values at move-time, so `scope = Base` reads `record.base_power`/
        // `record.base_toughness` while `scope = Current` reads
        // `record.power`/`record.toughness`. This makes the look-back
        // (leaves-the-battlefield / dies) path as precise as live-object
        // battlefield filtering (CR 603.10a): a base-1/1 with a +1/+1 counter
        // matches `power <= 1` under `Base` but not under `Current`.
        FilterProp::PtComparison {
            stat,
            scope,
            comparator,
            value,
        } => comparator.evaluate(
            zone_change_pt_value(record, *stat, *scope),
            resolve_filter_threshold(state, value, source),
        ),
        // CR 202.3: Mana value threshold on the event-time object.
        FilterProp::Cmc { comparator, value } => comparator.evaluate(
            record.mana_value as i32,
            resolve_filter_threshold(state, value, source),
        ),
        // CR 202.3 + CR 608.2c: The event-time mana value is fixed in the
        // snapshot; the chosen odd/even quality is read from resolution state.
        FilterProp::ManaValueParity { parity } => {
            mana_value_matches_parity_source(record.mana_value, parity, state.last_named_choice.as_ref())
        }
        // CR 202.1: Zone-change records currently snapshot mana value, not the
        // full printed mana cost. Exact-cost predicates fail closed here.
        FilterProp::ManaCostIn { .. } => false,
        // CR 105.1 / CR 202.2: Color membership on the event-time object.
        FilterProp::HasColor { color } => record.colors.contains(color),
        FilterProp::NotColor { color } => !record.colors.contains(color),
        FilterProp::ColorCount { comparator, count } => {
            comparator.evaluate(record.colors.len() as i32, i32::from(*count))
        }
        // CR 208.1 / CR 107.2: `toughness > power` comparison on the snapshot.
        FilterProp::ToughnessGTPower => record.toughness.unwrap_or(0) > record.power.unwrap_or(0),
        // CR 208.1 + CR 613.4b: `power > base power` on the zone-change snapshot —
        // both characteristics are captured at event time, so a look-back
        // ("a creature ... with power greater than its base power deals combat
        // damage") evaluates faithfully against the record.
        FilterProp::PowerExceedsBase => {
            record.power.unwrap_or(0) > record.base_power.unwrap_or(0)
        }
        // CR 111.1: Token identity as of the zone change. Token-ness is a
        // stable property of the object, captured in the snapshot so that
        // "whenever a creature token dies" (Grismold) and similar LTB
        // triggers evaluate correctly after the token has moved to the
        // graveyard (and then ceased to exist per CR 111.7).
        FilterProp::Token => record.is_token,
        // CR 111.1 + CR 603.6a: Nontoken identity as of the zone change.
        FilterProp::NonToken => !record.is_token,
        // CR 305.1 + CR 601.2a: zone-change snapshots carry cast/play provenance
        // when the object was cast or played — not mere zone moves (reanimate).
        FilterProp::WasPlayed => {
            record.played_from_zone.is_some() || record.cast_from_zone.is_some()
        }

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
                source.controller.is_some()
                    && source.controller != Some(record.owner)
                    && super::players::is_alive(state, record.owner)
            }
            ControllerRef::ScopedPlayer => {
                scoped_player_or_controller(state, source.ability, source.controller, None)
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
            ControllerRef::ParentTargetController => {
                parent_target_controller_player(state, source.ability)
                    .is_some_and(|pid| pid == record.owner)
            }
            ControllerRef::ParentTargetOwner => parent_target_owner_player(state, source.ability)
                .is_some_and(|pid| pid == record.owner),
            ControllerRef::DefendingPlayer => {
                crate::game::combat::defending_player_for_attacker(state, source.id)
                    .is_some_and(|pid| pid == record.owner)
            }
            // CR 613.1: Ownership relative to the source's persisted chosen player.
            ControllerRef::SourceChosenPlayer => {
                crate::game::game_object::source_chosen_player(state, source.id)
                    .is_some_and(|pid| pid == record.owner)
            }
            // CR 608.2c + CR 109.4: Ownership relative to a resolution-chosen player.
            ControllerRef::ChosenPlayer { index } => source
                .ability
                .and_then(|a| a.chosen_players.get(*index as usize).copied())
                .is_some_and(|pid| pid == record.owner),
            // CR 603.2 + CR 109.4: Ownership relative to the triggering player.
            ControllerRef::TriggeringPlayer => {
                crate::game::quantity::triggering_event_player(state).is_some_and(|pid| pid == record.owner)
            }
        },
        // CR 205.3e + CR 205.3m + CR 702.73a: Source's chosen creature type
        // applied to the snapshot subtypes, including changeling snapshots.
        FilterProp::IsChosenCreatureType => source.chosen_creature_type.is_some_and(|chosen| {
            subtype_matches_with_changeling(
                chosen,
                &record.subtypes,
                &record.keywords,
                &state.all_creature_types,
            )
        }),
        FilterProp::MostPrevalentCreatureTypeIn { .. } => false,
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
        FilterProp::Attacking { defender } => {
            record.combat_status.attacking
                && match defender {
                    None => true,
                    Some(defender) => record.combat_status.defending_player.is_some_and(
                        |defending_player| {
                            attacking_defender_matches(state, source, defending_player, Some(defender))
                        },
                    ),
                }
        }
        FilterProp::Blocking => record.combat_status.blocking,
        // `ZoneChangeCombatStatus` snapshots role, not the blocker-to-attacker
        // relation. Source-relative blocker checks require live combat state.
        FilterProp::BlockingSource | FilterProp::CombatRelation { .. } => false,
        FilterProp::Unblocked => {
            record.combat_status.attacking && !record.combat_status.blocked
        }
        // CR 506.5 + CR 603.10a: sole-attacker / sole-blocker status as of the
        // zone change, captured by `capture_combat_status` before combat removal.
        FilterProp::AttackingAlone => record.combat_status.attacking_alone,
        FilterProp::BlockingAlone => record.combat_status.blocking_alone,
        FilterProp::HasAttachment {
            kind,
            controller,
            exclude_source,
        } => record.attachments.iter().any(|att| {
            (exclude_source.is_include() || att.object_id != source.id)
                && att.kind == *kind
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
        // CR 702.95b: Pairing exists only between battlefield creatures. For
        // a battlefield zone-change event, consult the live object after entry
        // so Soulbond's "another unpaired creature enters" trigger can see the
        // entering creature before any pair-forming effect resolves.
        FilterProp::Unpaired => state
            .objects
            .get(&record.object_id)
            .is_some_and(|obj| obj.paired_with.is_none()),

        // These predicates query live battlefield state (tap status, attachment,
        // current counters, face-down). The snapshot has already left its public
        // zone, so the predicate is semantically not applicable.
        FilterProp::Counters {
            counters,
            comparator,
            count,
        } => state.lki_cache.get(&record.object_id).is_some_and(|lki| {
            let selector = match counters {
                CounterMatch::Any => None,
                CounterMatch::OfType(ct) => Some(ct),
            };
            let actual = counter_count_from_map(&lki.counters, selector);
            comparator.evaluate(actual, resolve_filter_threshold(state, count, source))
        }),
        FilterProp::Tapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::Untapped
        | FilterProp::HasHasteOrControlledSinceTurnBegan
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
        // CR 608.2c: Logical negation — recurse under the same record and invert.
        FilterProp::Not { prop } => {
            !zone_change_record_matches_property(prop, state, record, source)
        }

        // -------- Group 4: not-yet-supported (known conservative gaps) --------
        // These could be snapshotted (e.g. suspected status, damage-dealt-this-turn)
        // or require state joins that aren't plumbed to this evaluator. Expand as
        // trigger-filter coverage grows.
        FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::IsChosenLandOrNonlandKind
        | FilterProp::HasSingleTarget
        | FilterProp::Suspected
        | FilterProp::Renowned
        // CR 700.9: Modified is a live-battlefield predicate (counters +
        // attachments) — a zone-change snapshot cannot represent it.
        | FilterProp::Modified
        | FilterProp::DifferentNameFrom { .. }
        | FilterProp::InAnyZone { .. }
        | FilterProp::SharesQuality { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ZoneChangedThisTurn { .. }
        // CR 122.6: counters-put-this-turn is a live-history join keyed on the
        // object id; a zone-change snapshot does not carry it. Fail closed.
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::TargetsOnly { .. }
        | FilterProp::Targets { .. }
        // CR 107.3 + CR 202.1: X-in-cost is a spell-cast-time predicate; it has no
        // meaning for a zone-change record (the object has already left the stack
        // or never was a spell). Fail closed — the snapshot carries no such info.
        | FilterProp::HasXInManaCost
        | FilterProp::WasKicked
        | FilterProp::HasXInActivationCost
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
            scoped_player_or_controller(state, source.ability, source.controller, None)
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
        Some(ControllerRef::ParentTargetController) => {
            parent_target_controller_player(state, source.ability)
                .is_some_and(|pid| pid == attachment_controller)
        }
        Some(ControllerRef::ParentTargetOwner) => parent_target_owner_player(state, source.ability)
            .is_some_and(|pid| pid == attachment_controller),
        Some(ControllerRef::DefendingPlayer) => {
            combat::defending_player_for_attacker(state, source.id)
                .is_some_and(|pid| pid == attachment_controller)
        }
        // CR 613.1: Attachment controller relative to the source's chosen player.
        Some(ControllerRef::SourceChosenPlayer) => {
            crate::game::game_object::source_chosen_player(state, source.id)
                .is_some_and(|pid| pid == attachment_controller)
        }
        // CR 608.2c + CR 109.4: Attachment controller relative to a chosen player.
        Some(ControllerRef::ChosenPlayer { index }) => source
            .ability
            .and_then(|a| a.chosen_players.get(*index as usize).copied())
            .is_some_and(|pid| pid == attachment_controller),
        // CR 603.2 + CR 109.4: Attachment controller relative to the triggering player.
        Some(ControllerRef::TriggeringPlayer) => {
            crate::game::quantity::triggering_event_player(state)
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

struct SharedQualitySource<'a> {
    name: &'a str,
    power: Option<i32>,
    toughness: Option<i32>,
    mana_value: u32,
    core_types: &'a [CoreType],
    subtypes: &'a [String],
    colors: &'a [ManaColor],
    keywords: &'a [Keyword],
}

fn shared_quality_values(
    source: SharedQualitySource<'_>,
    quality: &SharedQuality,
    all_creature_types: &[String],
) -> HashSet<String> {
    match quality {
        SharedQuality::Name => {
            if source.name.is_empty() {
                HashSet::new()
            } else {
                HashSet::from([source.name.to_ascii_lowercase()])
            }
        }
        SharedQuality::ManaValue => HashSet::from([source.mana_value.to_string()]),
        SharedQuality::Power => source
            .power
            .map_or_else(HashSet::new, |value| HashSet::from([value.to_string()])),
        SharedQuality::Toughness => source
            .toughness
            .map_or_else(HashSet::new, |value| HashSet::from([value.to_string()])),
        SharedQuality::TotalPowerToughness => source
            .power
            .zip(source.toughness)
            .map_or_else(HashSet::new, |(power, toughness)| {
                HashSet::from([(power + toughness).to_string()])
            }),
        SharedQuality::CreatureType => {
            if source
                .keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Changeling))
            {
                return all_creature_types
                    .iter()
                    .map(|creature_type| creature_type.to_ascii_lowercase())
                    .collect();
            }

            source
                .subtypes
                .iter()
                .filter(|subtype| {
                    all_creature_types
                        .iter()
                        .any(|creature_type| subtype.eq_ignore_ascii_case(creature_type))
                })
                .map(|subtype| subtype.to_ascii_lowercase())
                .collect()
        }
        SharedQuality::Color => source
            .colors
            .iter()
            .map(|color| format!("{color:?}").to_ascii_lowercase())
            .collect(),
        SharedQuality::CardType => source
            .core_types
            .iter()
            .map(|card_type| format!("{card_type:?}").to_ascii_lowercase())
            .collect(),
        SharedQuality::LandType => source
            .subtypes
            .iter()
            .filter(|subtype| is_land_type(subtype))
            .map(|subtype| subtype.to_ascii_lowercase())
            .collect(),
    }
}

/// CR 201.2 + CR 603.4: Public re-export of the per-object quality extractor.
/// Used by the `QuantityRef::ObjectCountDistinct` resolver so the
/// count-expression side and the constraint side share one vocabulary for
/// `SharedQuality` value semantics.
pub fn object_shared_quality_values_public(
    obj: &GameObject,
    quality: &SharedQuality,
    all_creature_types: &[String],
) -> HashSet<String> {
    object_shared_quality_values(obj, quality, all_creature_types)
}

fn object_shared_quality_values(
    obj: &GameObject,
    quality: &SharedQuality,
    all_creature_types: &[String],
) -> HashSet<String> {
    shared_quality_values(
        SharedQualitySource {
            name: &obj.name,
            power: obj.power,
            toughness: obj.toughness,
            // CR 202.3e: For on-stack objects, X equals the announced value, not 0.
            mana_value: obj.mana_cost.mana_value_with_x(obj.zone, obj.cost_x_paid),
            core_types: &obj.card_types.core_types,
            subtypes: &obj.card_types.subtypes,
            colors: &obj.color,
            keywords: &obj.keywords,
        },
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
        SharedQualitySource {
            name: &lki.name,
            power: lki.power,
            toughness: lki.toughness,
            mana_value: lki.mana_value,
            core_types: &lki.card_types,
            subtypes: &lki.subtypes,
            colors: &lki.colors,
            keywords: &lki.keywords,
        },
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
        .or(source.recipient_id);

    // Resolution ladder: live object → target-id LKI → effect-context snapshot.
    // The first two rungs honor a genuinely chosen target; the snapshot rung is a
    // strict fallback reached only when both yield nothing — including when
    // `target_id` is `None` (untargeted parent) or `Some` but stale (missing from
    // both `state.objects` and `state.lki_cache`).
    target_id
        .and_then(|id| state.objects.get(&id))
        .map(|obj| object_shared_quality_values(obj, quality, &state.all_creature_types))
        .or_else(|| {
            target_id
                .and_then(|id| state.lki_cache.get(&id))
                .map(|lki| lki_shared_quality_values(lki, quality, &state.all_creature_types))
        })
        .or_else(|| {
            // CR 608.2k + CR 400.7j: `ParentTarget` may refer to a permanent the
            // parent effect sacrificed (an untargeted object never written into
            // `ability.targets`). The sacrifice moves it to the graveyard — a
            // public zone — so later instructions in the same effect still
            // resolve against it via the effect-context LKI snapshot.
            source
                .ability
                .and_then(|ability| ability.effect_context_object.as_ref())
                .map(|snapshot| {
                    lki_shared_quality_values(&snapshot.lki, quality, &state.all_creature_types)
                })
        })
}

fn evaluate_shares_quality(
    state: &GameState,
    obj: &GameObject,
    quality: &SharedQuality,
    reference: &Option<Box<TargetFilter>>,
    relation: &SharedQualityRelation,
    source: &SourceContext<'_>,
) -> bool {
    let shares = reference.as_ref().is_none_or(|reference_filter| {
        object_shares_quality_with_reference_filter(state, obj, quality, reference_filter, source)
    });
    match relation {
        SharedQualityRelation::Shares => shares,
        SharedQualityRelation::DoesNotShare => {
            !shares
                && (!matches!(quality, SharedQuality::Name)
                    || !object_shared_quality_values(obj, quality, &state.all_creature_types)
                        .is_empty())
        }
    }
}

fn source_context_from_spell_filter(context: SpellFilterContext<'_>) -> SourceContext<'_> {
    let source_obj = context.state.objects.get(&context.source_id);
    SourceContext {
        id: context.source_id,
        controller: Some(context.source_controller),
        attached_to: source_obj.and_then(|o| o.attached_to),
        source_is_aura: source_obj
            .is_some_and(|o| o.card_types.subtypes.iter().any(|s| s == "Aura")),
        source_is_equipment: source_obj
            .is_some_and(|o| o.card_types.subtypes.iter().any(|s| s == "Equipment")),
        chosen_creature_type: source_obj.and_then(|o| o.chosen_creature_type()),
        chosen_attributes: source_obj
            .map(|o| o.chosen_attributes.as_slice())
            .unwrap_or(&[]),
        ability: None,
        recipient_id: None,
    }
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

    let ctx = FilterContext {
        source_id: source.id,
        source_controller: source.controller,
        ability: source.ability,
        recipient_id: source.recipient_id,
        scoped_iteration_player: None,
    };
    state.objects.keys().copied().any(|reference_id| {
        filter_inner(state, reference_id, reference_filter, &ctx)
            && state
                .objects
                .get(&reference_id)
                .is_some_and(|reference_obj| {
                    let values = object_shared_quality_values(
                        reference_obj,
                        quality,
                        &state.all_creature_types,
                    );
                    object_shares_quality_values(obj, quality, &values, &state.all_creature_types)
                })
    })
}

/// CR 205.3m: Compute the creature subtypes tied for highest
/// occurrence among creature cards in `owner`'s `zone`. CR 205.3m defines
/// the creature-subtype set being counted. A `Changeling` (CR 702.73a)
/// creature counts toward every creature type, matching how the keyword
/// interacts with subtype-counting effects on resolution.
///
/// Owner semantics are correct for hidden zones (library, hand) and
/// graveyard/exile per CR 400 (zones are owned by players). Battlefield
/// emission, if/when added, would need an explicit controller axis since
/// owner ≠ controller for stolen permanents.
fn most_prevalent_creature_types_in_zone(
    state: &GameState,
    owner: PlayerId,
    zone: Zone,
) -> HashSet<String> {
    let object_ids = crate::game::targeting::zone_object_ids(state, zone);
    let mut counts: HashMap<String, u32> = HashMap::new();
    for object_id in object_ids {
        let Some(obj) = state.objects.get(&object_id) else {
            continue;
        };
        if obj.owner != owner {
            continue;
        }
        if !obj.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        if obj.keywords.contains(&Keyword::Changeling) {
            for creature_type in &state.all_creature_types {
                *counts
                    .entry(creature_type.to_ascii_lowercase())
                    .or_insert(0) += 1;
            }
            continue;
        }
        for subtype in &obj.card_types.subtypes {
            if state
                .all_creature_types
                .iter()
                .any(|creature_type| creature_type.eq_ignore_ascii_case(subtype))
            {
                *counts.entry(subtype.to_ascii_lowercase()).or_insert(0) += 1;
            }
        }
    }

    let max_count = counts.values().copied().max().unwrap_or(0);
    counts
        .into_iter()
        .filter_map(|(creature_type, count)| (count == max_count).then_some(creature_type))
        .collect()
}

/// CR 608.2b: Validate that all targeted objects share at least one value of the named quality.
/// This is a group constraint that cannot be checked per-object — it requires the full set.
/// Checked at resolution time per CR 608.2b (verifying target legality on resolution).
///
/// Returns `true` if the constraint is satisfied (or if there are fewer than 2 targets).
/// For "creature type": all objects must share at least one creature subtype.
/// For "color": all objects must share at least one color.
/// For "card type": all objects must share at least one card type.
/// CR 608.2c + CR 201.2: True when two objects share at least one value of the
/// named quality. Used by `AbilityCondition::ObjectsShareQuality`.
pub fn objects_share_quality(
    state: &GameState,
    left: ObjectId,
    right: ObjectId,
    quality: &SharedQuality,
) -> bool {
    let Some(left_obj) = state.objects.get(&left) else {
        return false;
    };
    let Some(right_obj) = state.objects.get(&right) else {
        return false;
    };
    let left_vals = object_shared_quality_values(left_obj, quality, &state.all_creature_types);
    let right_vals = object_shared_quality_values(right_obj, quality, &state.all_creature_types);
    !left_vals.is_disjoint(&right_vals)
}

/// CR 608.2b: Validate that all targeted objects share at least one value of the named quality.
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
    player_matches_target_filter_with(
        filter,
        player_id,
        source_controller,
        &|controller, player| controller != player,
    )
}

/// Check if a player target matches a TargetFilter constraint using team-aware
/// opponent semantics from the game state.
/// CR 102.2 / CR 102.3 / CR 115.9c: Opponent-scoped player targets exclude
/// teammates in team multiplayer.
pub fn player_matches_target_filter_in_state(
    state: &GameState,
    filter: &TargetFilter,
    player_id: PlayerId,
    source_controller: Option<PlayerId>,
) -> bool {
    player_matches_target_filter_with(
        filter,
        player_id,
        source_controller,
        &|controller, player| crate::game::players::is_opponent(state, controller, player),
    )
}

fn player_matches_target_filter_with(
    filter: &TargetFilter,
    player_id: PlayerId,
    source_controller: Option<PlayerId>,
    is_opponent: &impl Fn(PlayerId, PlayerId) -> bool,
) -> bool {
    match filter {
        TargetFilter::Any | TargetFilter::Player => true,
        TargetFilter::SelfRef => false, // SelfRef refers to objects, not players
        TargetFilter::Controller => source_controller == Some(player_id),
        // CR 109.5: Without ability context, OriginalController is indistinguishable
        // from Controller — both refer to the source controller in this matcher.
        TargetFilter::OriginalController => source_controller == Some(player_id),
        TargetFilter::ScopedPlayer => false,
        TargetFilter::Typed(ref tf) if tf.type_filters.is_empty() => match &tf.controller {
            Some(ControllerRef::You) => source_controller == Some(player_id),
            Some(ControllerRef::Opponent) => {
                source_controller.is_some_and(|controller| is_opponent(controller, player_id))
            }
            Some(ControllerRef::ScopedPlayer) => false,
            // CR 109.4: TargetPlayer has no meaning when matching a player against
            // a filter without ability context. Fail closed (mirrors the pattern
            // established at filter.rs:526–569 for spell-record filters).
            Some(ControllerRef::TargetPlayer) => false,
            Some(ControllerRef::ParentTargetController) => false,
            Some(ControllerRef::ParentTargetOwner) => false,
            Some(ControllerRef::DefendingPlayer) => false,
            // CR 613.1: "the chosen player" has no meaning in this name-filter
            // context. Fail closed (mirrors `TargetPlayer`).
            Some(ControllerRef::SourceChosenPlayer) => false,
            // CR 109.4: Chosen-player scope has no meaning without resolution
            // context. Fail closed (mirrors `TargetPlayer`).
            Some(ControllerRef::ChosenPlayer { .. }) => false,
            // CR 603.2 + CR 109.4: Triggering-player scope has no meaning
            // without event/game-state context here. Fail closed.
            Some(ControllerRef::TriggeringPlayer) => false,
            None => true,
        },
        // Typed filters with type_filters don't match players
        TargetFilter::Typed(_) => false,
        TargetFilter::Or { filters } => filters.iter().any(|f| {
            player_matches_target_filter_with(f, player_id, source_controller, is_opponent)
        }),
        TargetFilter::And { filters } => filters.iter().all(|f| {
            player_matches_target_filter_with(f, player_id, source_controller, is_opponent)
        }),
        // CR 102.1 + CR 103.1: seating-neighbor resolution requires
        // `state.seat_order`, which is not available in this stateless matcher.
        // The recipient is resolved upstream at the GainControl recipient path
        // (`gain_control::unique_recipient_from_filter`). Fail closed here —
        // mirrors the `TriggeringPlayer` / `TargetPlayer` fail-closed arms.
        TargetFilter::Neighbor { .. } => false,
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
        PlayerScope, QuantityExpr, QuantityRef, ReplacementDefinition, ResolvedAbility,
        StaticDefinition, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::events::GameEvent;
    use crate::types::format::FormatConfig;
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
    fn cmc_filter_treats_retained_x_as_zero_off_stack() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let x_creature = add_creature(&mut state, PlayerId(0), "Endless One");
        {
            let obj = state.objects.get_mut(&x_creature).unwrap();
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
            // CR 107.3m: X paid survives stack -> battlefield for ETB
            // replacement/trigger logic, but CR 202.3e still treats X as 0
            // once the object is no longer on the stack.
            obj.cost_x_paid = Some(4);
        }

        let mana_value_four_or_more =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            }]));

        assert!(
            !matches_target_filter(&state, x_creature, &mana_value_four_or_more, source),
            "battlefield X permanent retains cost_x_paid for ETB logic, but its mana value is 0"
        );

        state.objects.get_mut(&x_creature).unwrap().zone = Zone::Stack;
        assert!(
            matches_target_filter(&state, x_creature, &mana_value_four_or_more, source),
            "the same X object on the stack must include the announced X value"
        );
    }

    /// CR 112.1 + CR 113.3b/113.3c: the bare-spell leg emitted by the
    /// stack-object combinator — `Typed { [Card], InZone{Stack} }` — must be
    /// runtime-equivalent to `StackSpell` for legality: it matches a spell
    /// stack entry (a card object registered in `state.objects`) but NOT an
    /// ability stack entry (whose entry id is never inserted as an object).
    /// This locks the representation change in
    /// `parse_ability_spell_disjunction`'s bare-spell leg.
    #[test]
    fn bare_spell_stack_leg_matches_spell_not_ability() {
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".into(),
            Zone::Stack,
        );

        // A spell stack entry: a card object placed on the stack, with a
        // matching `StackEntry { id == card object id, kind: Spell }`.
        let spell_card_id = CardId(state.next_object_id);
        let spell_obj = create_object(
            &mut state,
            spell_card_id,
            PlayerId(0),
            "Some Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: spell_obj,
            source_id: spell_obj,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: spell_card_id,
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // An ability stack entry: a fresh entry id that is NOT inserted into
        // `state.objects` (mirrors the real trigger-push path, which allocates
        // the entry id from `next_object_id` without creating an object).
        let ability_entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: ability_entry_id,
            source_id: source,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });

        // The bare-spell leg as emitted by the combinator.
        let bare_spell_leg = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Card],
            controller: None,
            properties: vec![FilterProp::InZone { zone: Zone::Stack }],
        });
        let ctx = FilterContext::from_source_with_controller(source, PlayerId(0));

        assert!(
            matches_stack_target_filter(&state, spell_obj, &bare_spell_leg, &ctx),
            "the bare-spell leg must match a spell stack entry (a card object on the stack)"
        );
        assert!(
            !matches_stack_target_filter(&state, ability_entry_id, &bare_spell_leg, &ctx),
            "the bare-spell leg must NOT match an ability stack entry (no card object exists for it)"
        );
        // And the StackAbility leg has the complementary behavior — it matches
        // the ability but not the spell, so the Or covers both disjointly.
        let ability_leg = TargetFilter::StackAbility {
            controller: None,
            tag: None,
            kind: None,
        };
        assert!(
            matches_stack_target_filter(&state, ability_entry_id, &ability_leg, &ctx),
            "the StackAbility leg must match the ability stack entry"
        );
        assert!(
            !matches_stack_target_filter(&state, spell_obj, &ability_leg, &ctx),
            "the StackAbility leg must NOT match the spell stack entry"
        );
    }

    #[test]
    fn none_filter_matches_nothing() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(!matches_target_filter(&state, id, &TargetFilter::None, id));
    }

    #[test]
    fn player_filter_in_state_excludes_two_headed_giant_teammate_for_opponent_scope() {
        let state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        let opponent_filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));

        assert!(!player_matches_target_filter_in_state(
            &state,
            &opponent_filter,
            PlayerId(1),
            Some(PlayerId(0))
        ));
        assert!(player_matches_target_filter_in_state(
            &state,
            &opponent_filter,
            PlayerId(2),
            Some(PlayerId(0))
        ));
    }

    /// CR 702.26b: `matches_target_filter_including_phased_out` evaluates the
    /// filter against phased-out permanents (which the normal choke point hides)
    /// while still honoring controller scope — the basis for filtered mass
    /// phase-in.
    #[test]
    fn including_phased_out_matches_controller_scoped_phased_out_object() {
        use crate::types::ability::TypedFilter;

        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");
        for id in [mine, theirs] {
            state.objects.get_mut(&id).unwrap().phase_status =
                crate::game::game_object::PhaseStatus::PhasedOut {
                    cause: crate::game::game_object::PhaseOutCause::Directly,
                };
        }

        let you = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
        let ctx = FilterContext::from_source_with_controller(mine, PlayerId(0));

        // The regular choke point excludes phased-out objects entirely.
        assert!(!super::matches_target_filter(&state, mine, &you, &ctx));
        // The phased-out-aware matcher matches the controller's phased-out
        // object, but still respects controller scope (opponent's is excluded).
        assert!(super::matches_target_filter_including_phased_out(
            &state, mine, &you, &ctx
        ));
        assert!(!super::matches_target_filter_including_phased_out(
            &state, theirs, &you, &ctx
        ));
    }

    /// Issue #1747 (perf): `matches_target_filter_in_owner_zone` skips the
    /// `GameObject` clone when `controller == owner` (the fast path), and only
    /// clones to override `controller := owner` when they differ (the slow
    /// path). Both paths MUST yield the owner-scoped result — CR 109.5 / CR
    /// 400.3: a card in an owner zone is evaluated with ownership standing in
    /// for controller, so a control-changed card still counts as its owner's.
    #[test]
    fn owner_zone_filter_scopes_to_owner_on_fast_and_slow_paths() {
        use crate::types::ability::TypedFilter;

        let mut state = setup();
        // A card in P0's library, owned AND controlled by P0 → fast path.
        let cid = CardId(state.next_object_id);
        let card = create_object(
            &mut state,
            cid,
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        let your_card = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
        let ctx_p0 = FilterContext::from_source_with_controller(card, PlayerId(0));
        state.lki_cache.insert(
            card,
            LKISnapshot {
                name: "Forest".to_string(),
                power: None,
                toughness: None,
                base_power: None,
                base_toughness: None,
                mana_value: 0,
                controller: PlayerId(1),
                owner: PlayerId(0),
                card_types: vec![CoreType::Land],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: vec![],
                counters: Default::default(),
            },
        );

        // Fast path: controller == owner == P0, no clone needed. The stale LKI
        // controller differs, so owner-zone matching must force the live
        // owner-scoped controller instead of reading `state.lki_cache`.
        assert_eq!(
            state.objects[&card].controller, state.objects[&card].owner,
            "precondition: fast path requires controller == owner"
        );
        assert!(
            super::matches_target_filter_in_owner_zone(&state, card, &your_card, &ctx_p0),
            "owner-zone card owned+controlled by P0 is P0's card"
        );

        // Slow path: control-change the card to P1 (owner stays P0). Owner-
        // scoping must still treat it as P0's card via the clone+override,
        // even though the LKI controller also names P1.
        state.objects.get_mut(&card).unwrap().controller = PlayerId(1);
        assert_ne!(
            state.objects[&card].controller, state.objects[&card].owner,
            "precondition: slow path requires controller != owner"
        );
        assert!(
            super::matches_target_filter_in_owner_zone(&state, card, &your_card, &ctx_p0),
            "owner-scoping: a P1-controlled, P0-owned card in an owner zone is still P0's"
        );
    }

    #[test]
    fn any_filter_matches_everything() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(matches_target_filter(&state, id, &TargetFilter::Any, id));
    }

    #[test]
    fn pt_comparison_total_power_toughness_matches_sum() {
        use crate::types::ability::{PtStat, PtValueScope, TypedFilter};

        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Summed Bear");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);

        let total_filter = |scope, comparator, value| {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::TotalPowerToughness,
                    scope,
                    comparator,
                    value: QuantityExpr::Fixed { value },
                },
            ]))
        };

        assert!(!matches_target_filter(
            &state,
            id,
            &total_filter(PtValueScope::Current, Comparator::LE, 5),
            id,
        ));
        assert!(matches_target_filter(
            &state,
            id,
            &total_filter(PtValueScope::Current, Comparator::GE, 6),
            id,
        ));
        assert!(matches_target_filter(
            &state,
            id,
            &total_filter(PtValueScope::Base, Comparator::LE, 4),
            id,
        ));
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
    fn has_haste_or_controlled_since_turn_began_matches_enlist_eligibility() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Enlister");
        let established = add_creature(&mut state, PlayerId(0), "Established");
        let sick = add_creature(&mut state, PlayerId(0), "Fresh");
        let hasty = add_creature(&mut state, PlayerId(0), "Hasty");
        let land = add_creature(&mut state, PlayerId(0), "Animated Land");

        state.objects.get_mut(&sick).unwrap().summoning_sick = true;
        {
            let obj = state.objects.get_mut(&hasty).unwrap();
            obj.summoning_sick = true;
            obj.keywords.push(Keyword::Haste);
        }
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];

        let filter = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::HasHasteOrControlledSinceTurnBegan]),
        );

        assert!(matches_target_filter(&state, established, &filter, source));
        assert!(
            !matches_target_filter(&state, sick, &filter, source),
            "summoning-sick creature without haste is not eligible for Enlist"
        );
        assert!(
            matches_target_filter(&state, hasty, &filter, source),
            "haste satisfies CR 702.154a even when the creature entered this turn"
        );
        assert!(
            !matches_target_filter(&state, land, &filter, source),
            "predicate is creature-specific when used without an outer creature filter"
        );
    }

    /// CR 120.6 + CR 120.9 (audit H2): "Was dealt damage this turn" must consult
    /// the damage-event history, not `damage_marked`. Per CR 120.6 marked damage
    /// is removed when the permanent regenerates, but the historical fact (CR 120.9)
    /// survives — so a creature that was dealt damage and then regenerated must
    /// still be a legal target for "destroy target creature that was dealt damage
    /// this turn" (Fatal Blow). The pre-fix implementation read `damage_marked`
    /// and silently lost the fact.
    #[test]
    fn was_dealt_damage_this_turn_survives_regeneration() {
        use crate::types::game_state::DamageRecord;

        let mut state = setup();
        let creature = add_creature(&mut state, PlayerId(0), "Wall of Resistance");
        let damage_source = add_creature(&mut state, PlayerId(1), "Goblin Piker");

        // Push the historical record, then simulate regeneration (CR 120.6:
        // "All damage marked on a permanent is removed when it regenerates").
        state.damage_dealt_this_turn.push_back(DamageRecord {
            source_id: damage_source,
            source_controller: PlayerId(1),
            target: TargetRef::Object(creature),
            target_controller: PlayerId(0),
            amount: 2,
            is_combat: true,
            ..Default::default()
        });
        state.objects.get_mut(&creature).unwrap().damage_marked = 0;

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::WasDealtDamageThisTurn]),
        );
        assert!(
            matches_target_filter(&state, creature, &filter, creature),
            "Fatal Blow target must remain legal after the creature regenerates"
        );

        // Negative control: an undamaged creature does not match.
        let untouched = add_creature(&mut state, PlayerId(0), "Grizzly Bears");
        assert!(!matches_target_filter(
            &state, untouched, &filter, untouched
        ));
    }

    #[test]
    fn spell_record_matches_qualified_filter() {
        let record = SpellCastRecord {
            name: String::new(),
            core_types: vec![CoreType::Creature],
            supertypes: vec![Supertype::Legendary],
            subtypes: vec!["Bird".to_string()],
            keywords: vec![Keyword::Flying],
            colors: vec![ManaColor::Blue],
            mana_value: 3,
            has_x_in_cost: false,
            from_zone: Zone::Hand,
            cast_variant: crate::types::game_state::CastingVariant::Normal,
            was_kicked: false,
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
            name: String::new(),
            core_types: vec![CoreType::Creature],
            supertypes: vec![],
            subtypes: vec![],
            keywords: vec![],
            colors: vec![],
            mana_value: 3,
            has_x_in_cost: true,
            from_zone: Zone::Hand,
            cast_variant: crate::types::game_state::CastingVariant::Normal,
            was_kicked: false,
        };
        let non_x_record = SpellCastRecord {
            has_x_in_cost: false,
            from_zone: Zone::Hand,
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

    /// CR 107.3 + CR 601.2f: `FilterProp::HasXInActivationCost` consults the
    /// pending activation record and composes with typed filters via `And`.
    #[test]
    fn pending_activation_has_x_in_activation_cost_composes_with_type_filter() {
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter as Tf,
            TypedFilter,
        };
        use crate::types::mana::{ManaCost, ManaCostShard};

        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "X Activator");
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: Tf::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::X],
            },
        });
        std::sync::Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities)
            .push(ability);
        state.pending_activations.push((source, 0));

        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature()),
                TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::HasXInActivationCost]),
                ),
            ],
        };
        assert!(
            matches_target_filter(&state, source, &filter, source),
            "creature source with pending X activation must match composed filter"
        );
        state.pending_activations.clear();
        assert!(
            !matches_target_filter(&state, source, &filter, source),
            "without pending activation, HasXInActivationCost must fail"
        );
    }

    #[test]
    fn spell_record_matches_cast_origin_zone_filter() {
        let hand_record = SpellCastRecord {
            name: String::new(),
            core_types: vec![CoreType::Creature],
            supertypes: vec![],
            subtypes: vec![],
            keywords: vec![],
            colors: vec![],
            mana_value: 2,
            has_x_in_cost: false,
            from_zone: Zone::Hand,
            cast_variant: crate::types::game_state::CastingVariant::Normal,
            was_kicked: false,
        };
        let exile_record = SpellCastRecord {
            from_zone: Zone::Exile,
            ..hand_record.clone()
        };
        let filter = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
        );
        assert!(spell_record_matches_filter(
            &hand_record,
            &filter,
            PlayerId(0),
            &[]
        ));
        assert!(!spell_record_matches_filter(
            &exile_record,
            &filter,
            PlayerId(0),
            &[]
        ));
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

    // CR 702.171b: `IsSaddled` matches only objects with the saddled designation.
    #[test]
    fn is_saddled_property_matches_only_saddled() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Mount");

        let filter =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::IsSaddled]));

        // Not saddled → no match.
        assert!(!matches_target_filter(&state, id, &filter, id));

        // Saddled → match.
        state.objects.get_mut(&id).unwrap().is_saddled = true;
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
            .extend(["Elf".to_string(), "Warrior".to_string()]);

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

        let filter = TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Attacking { defender: None }]),
        );

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
    fn combat_relation_matches_creatures_blocking_or_blocked_by_parent_target() {
        use crate::game::combat::{AttackerInfo, CombatState};

        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let target_attacker = add_creature(&mut state, PlayerId(0), "Target Attacker");
        let target_blocker = add_creature(&mut state, PlayerId(1), "Target Blocker");
        let unrelated_attacker = add_creature(&mut state, PlayerId(0), "Unrelated Attacker");
        let unrelated_blocker = add_creature(&mut state, PlayerId(1), "Unrelated Blocker");
        state.combat = Some(CombatState {
            attackers: vec![
                AttackerInfo::attacking_player(target_attacker, PlayerId(1)),
                AttackerInfo::attacking_player(unrelated_attacker, PlayerId(1)),
            ],
            blocker_assignments: [
                (target_attacker, vec![target_blocker]),
                (unrelated_attacker, vec![unrelated_blocker]),
            ]
            .into(),
            blocker_to_attacker: [
                (target_blocker, vec![target_attacker]),
                (unrelated_blocker, vec![unrelated_attacker]),
            ]
            .into(),
            ..CombatState::default()
        });
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Object(target_attacker)],
            source,
            PlayerId(0),
        );
        let ctx = FilterContext::from_ability(&ability);
        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::CombatRelation {
                relation: CombatRelation::BlockingOrBlockedBy,
                subject: CombatRelationSubject::ParentTarget,
            },
        ]));

        assert!(crate::game::filter::matches_target_filter(
            &state,
            target_blocker,
            &filter,
            &ctx
        ));
        assert!(!crate::game::filter::matches_target_filter(
            &state,
            unrelated_blocker,
            &filter,
            &ctx
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
    fn typed_exiled_by_source_matches_only_linked_exiled_cards() {
        use crate::types::game_state::{ExileLink, ExileLinkKind};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let linked_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Linked Creature".into(),
            Zone::Exile,
        );
        let unlinked_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Unlinked Creature".into(),
            Zone::Exile,
        );
        let battlefield_creature = add_creature(&mut state, PlayerId(1), "Battlefield Creature");

        for id in [linked_creature, unlinked_creature] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        // CR 607.2a: "exiled this way" targets are linked to cards exiled by
        // the same source, not every object matching the typed phrase.
        state.exile_links.push(ExileLink {
            exiled_id: linked_creature,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });

        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature()),
                TargetFilter::ExiledBySource,
            ],
        };

        assert!(matches_target_filter(
            &state,
            linked_creature,
            &filter,
            source
        ));
        assert!(!matches_target_filter(
            &state,
            unlinked_creature,
            &filter,
            source
        ));
        assert!(!matches_target_filter(
            &state,
            battlefield_creature,
            &filter,
            source
        ));
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
    fn most_prevalent_creature_type_in_library_matches_highest_count_type() {
        let mut state = setup();
        state.all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];

        let goblin_one = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Goblin One".to_string(),
            Zone::Library,
        );
        let goblin_two = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin Two".to_string(),
            Zone::Library,
        );
        let elf = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Library,
        );
        for (id, subtype) in [(goblin_one, "Goblin"), (goblin_two, "Goblin"), (elf, "Elf")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push(subtype.to_string());
        }

        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::MostPrevalentCreatureTypeIn {
                zone: Zone::Library,
                scope: ControllerRef::You,
            },
        ]));

        assert!(matches_target_filter(
            &state, goblin_one, &filter, goblin_one
        ));
        assert!(matches_target_filter(
            &state, goblin_two, &filter, goblin_two
        ));
        assert!(!matches_target_filter(&state, elf, &filter, elf));
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
    fn shares_total_power_toughness_with_parent_target_matches_per_object() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Wild Pair");
        let entered = add_creature(&mut state, PlayerId(0), "Entered Creature");
        {
            let obj = state.objects.get_mut(&entered).unwrap();
            obj.power = Some(2);
            obj.toughness = Some(3);
        }
        let matching = add_creature(&mut state, PlayerId(0), "Matching Creature");
        {
            let obj = state.objects.get_mut(&matching).unwrap();
            obj.power = Some(4);
            obj.toughness = Some(1);
        }
        let nonmatching = add_creature(&mut state, PlayerId(0), "Nonmatching Creature");
        {
            let obj = state.objects.get_mut(&nonmatching).unwrap();
            obj.power = Some(3);
            obj.toughness = Some(3);
        }
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![TargetRef::Object(entered)],
            source,
            PlayerId(0),
        );
        let ctx = FilterContext::from_ability(&ability);
        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::SharesQuality {
                quality: SharedQuality::TotalPowerToughness,
                reference: Some(Box::new(TargetFilter::ParentTarget)),
                relation: SharedQualityRelation::Shares,
            },
        ]));

        assert!(super::matches_target_filter(
            &state, matching, &filter, &ctx
        ));
        assert!(!super::matches_target_filter(
            &state,
            nonmatching,
            &filter,
            &ctx
        ));
    }

    #[test]
    fn objects_share_quality_matches_shared_card_type() {
        let mut state = setup();
        let creature_a = add_creature(&mut state, PlayerId(0), "Creature A");
        let creature_b = add_creature(&mut state, PlayerId(0), "Creature B");

        assert!(super::objects_share_quality(
            &state,
            creature_a,
            creature_b,
            &SharedQuality::CardType,
        ));
        let instant = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Instant A".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let land = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Land A".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        assert!(!super::objects_share_quality(
            &state,
            creature_a,
            land,
            &SharedQuality::CardType,
        ));
        assert!(!super::objects_share_quality(
            &state,
            creature_a,
            instant,
            &SharedQuality::CardType,
        ));
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
            source_id: None,
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
            source_id: None,
        });
        state.current_trigger_events = vec![
            GameEvent::Discarded {
                player_id: PlayerId(0),
                object_id: discarded_creature,
                source_id: None,
            },
            GameEvent::Discarded {
                player_id: PlayerId(0),
                object_id: discarded_instant,
                source_id: None,
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

    /// CR 608.2c: `FilterProp::Not` building block — a single negated prop
    /// matches exactly the objects for which the inner prop does NOT hold.
    #[test]
    fn not_attacked_this_turn_matches_only_non_attackers() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        let idle = add_creature(&mut state, PlayerId(0), "Idle");
        state.creatures_attacked_this_turn.insert(attacker);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn),
            }]));

        assert!(!matches_target_filter(&state, attacker, &filter, attacker));
        assert!(matches_target_filter(&state, idle, &filter, attacker));
    }

    /// De Morgan: `[Not(Attacked), Not(Entered)]` AND-combines, so it matches
    /// only the creature that neither attacked nor entered this turn — the
    /// exact narrowing The Fifth Doctor's relative clause requires.
    /// NOTE: `add_creature` (via `create_object`) stamps
    /// `entered_battlefield_turn = Some(turn)`, so the pre-existing "veteran"
    /// and "attacker" have that field cleared to `None`; only the newcomer
    /// keeps the current-turn stamp.
    #[test]
    fn not_attacked_and_not_entered_matches_only_idle_veteran() {
        let mut state = setup();
        let attacker = add_creature(&mut state, PlayerId(0), "Attacker");
        let newcomer = add_creature(&mut state, PlayerId(0), "Newcomer");
        let veteran = add_creature(&mut state, PlayerId(0), "Veteran");
        state.creatures_attacked_this_turn.insert(attacker);
        // Veteran and attacker are pre-existing — they did NOT enter this turn.
        state
            .objects
            .get_mut(&veteran)
            .unwrap()
            .entered_battlefield_turn = None;
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .entered_battlefield_turn = None;
        // Newcomer entered this turn (already stamped by create_object).
        assert_eq!(
            state
                .objects
                .get(&newcomer)
                .unwrap()
                .entered_battlefield_turn,
            Some(state.turn_number)
        );

        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn),
            },
            FilterProp::Not {
                prop: Box::new(FilterProp::EnteredThisTurn),
            },
        ]));

        // Attacker: attacked → excluded. Newcomer: entered → excluded.
        // Veteran: neither → the only match.
        assert!(!matches_target_filter(&state, attacker, &filter, attacker));
        assert!(!matches_target_filter(&state, newcomer, &filter, attacker));
        assert!(matches_target_filter(&state, veteran, &filter, attacker));
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

    /// CR 201.2: HasChosenName must compare names case-insensitively to
    /// match the spell-cast prohibition path (`cant_cast_filter_matches`).
    /// Without parity Pithing Needle would silently miss target sources whose
    /// name differs from the player UI prompt only by casing.
    #[test]
    fn has_chosen_name_matches_case_insensitively() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Sorcerer");
        let bolt = add_creature(&mut state, PlayerId(0), "Lightning Bolt");

        // Player typed all-lowercase — must still match the printed name "Lightning Bolt".
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::CardName("lightning bolt".to_string()));

        assert!(matches_target_filter(
            &state,
            bolt,
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

    /// CR 208 + CR 107.3a: `PtComparison { Power, Current, LE, Variable("X") }`
    /// + `chosen_x = Some(3)` matches only power-≤-3 creatures.
    #[test]
    fn filter_context_from_ability_resolves_x_in_power_le() {
        use crate::types::ability::{
            Comparator, Effect, PtStat, PtValueScope, QuantityExpr, QuantityRef, ResolvedAbility,
            TargetFilter, TypedFilter,
        };
        let mut state = setup();
        let weak = add_creature(&mut state, PlayerId(0), "Weak");
        state.objects.get_mut(&weak).unwrap().power = Some(2);
        let strong = add_creature(&mut state, PlayerId(0), "Strong");
        state.objects.get_mut(&strong).unwrap().power = Some(5);

        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
        ]));
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

    #[test]
    fn can_enchant_matches_aura_keyword_against_parent_target() {
        let mut state = setup();
        let creature = add_creature(&mut state, PlayerId(0), "Host Creature");
        let aura = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Creature Aura".to_string(),
            Zone::Library,
        );
        {
            let aura_obj = state.objects.get_mut(&aura).unwrap();
            aura_obj.card_types.core_types.push(CoreType::Enchantment);
            aura_obj.card_types.subtypes.push("Aura".to_string());
            aura_obj.keywords.push(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )));
        }
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(creature)],
            ObjectId(999),
            PlayerId(0),
        );
        let filter =
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment).properties(vec![
                FilterProp::CanEnchant {
                    target: Box::new(TargetFilter::ParentTarget),
                },
            ]));
        let ctx = FilterContext::from_ability(&ability);

        assert!(super::matches_target_filter(&state, aura, &filter, &ctx));
    }

    #[test]
    fn can_enchant_rejects_aura_that_cannot_enchant_parent_target() {
        let mut state = setup();
        let creature = add_creature(&mut state, PlayerId(0), "Host Creature");
        let aura = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Land Aura".to_string(),
            Zone::Library,
        );
        {
            let aura_obj = state.objects.get_mut(&aura).unwrap();
            aura_obj.card_types.core_types.push(CoreType::Enchantment);
            aura_obj.card_types.subtypes.push("Aura".to_string());
            aura_obj
                .keywords
                .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::land())));
        }
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(creature)],
            ObjectId(999),
            PlayerId(0),
        );
        let filter =
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment).properties(vec![
                FilterProp::CanEnchant {
                    target: Box::new(TargetFilter::ParentTarget),
                },
            ]));
        let ctx = FilterContext::from_ability(&ability);

        assert!(!super::matches_target_filter(&state, aura, &filter, &ctx));
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

    /// CR 122.1: `Counters { count: Variable("X") }` + `chosen_x = Some(2)` matches
    /// only objects with ≥2 counters of the tracked type.
    #[test]
    fn filter_context_from_ability_resolves_x_in_counters_ge() {
        use crate::types::ability::{
            Comparator, Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
            TypedFilter,
        };
        use crate::types::counter::{CounterMatch, CounterType};
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
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                    comparator: Comparator::GE,
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

    /// #526 Wave Goodbye: `Counters { OfType(Plus1Plus1), EQ, Fixed(0) }`
    /// matches only creatures with zero +1/+1 counters. CR 122.1.
    #[test]
    fn counters_eq_zero_typed_matches_counterless_creature() {
        use crate::types::ability::{Comparator, QuantityExpr, TargetFilter, TypedFilter};
        use crate::types::counter::{CounterMatch, CounterType};
        let mut state = setup();
        let with = add_creature(&mut state, PlayerId(0), "WithCounter");
        state
            .objects
            .get_mut(&with)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        let without = add_creature(&mut state, PlayerId(0), "Bare");

        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                    comparator: Comparator::EQ,
                    count: QuantityExpr::Fixed { value: 0 },
                }]),
            );
        let ctx = FilterContext::from_source_with_controller(ObjectId(999), PlayerId(0));
        assert!(!super::matches_target_filter(&state, with, &filter, &ctx));
        assert!(super::matches_target_filter(&state, without, &filter, &ctx));
    }

    /// #527 Damning Verdict: `Counters { Any, EQ, Fixed(0) }` matches only
    /// creatures with no counters of ANY type — exercises `CounterMatch::Any`
    /// summing across every counter type. CR 122.1.
    #[test]
    fn counters_eq_zero_any_matches_only_uncounted_creature() {
        use crate::types::ability::{Comparator, QuantityExpr, TargetFilter, TypedFilter};
        use crate::types::counter::{CounterMatch, CounterType};
        let mut state = setup();
        let stunned = add_creature(&mut state, PlayerId(0), "Stunned");
        state
            .objects
            .get_mut(&stunned)
            .unwrap()
            .counters
            .insert(CounterType::Stun, 1);
        let pumped = add_creature(&mut state, PlayerId(0), "Pumped");
        state
            .objects
            .get_mut(&pumped)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);
        let bare = add_creature(&mut state, PlayerId(0), "Bare");

        let filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::EQ,
                    count: QuantityExpr::Fixed { value: 0 },
                }]),
            );
        let ctx = FilterContext::from_source_with_controller(ObjectId(999), PlayerId(0));
        assert!(!super::matches_target_filter(
            &state, stunned, &filter, &ctx
        ));
        assert!(!super::matches_target_filter(&state, pumped, &filter, &ctx));
        assert!(super::matches_target_filter(&state, bare, &filter, &ctx));
    }

    /// `CounterMatch::Any` truly SUMS across counter types: a creature with one
    /// Stun + one +1/+1 counter satisfies `{ Any, GE, Fixed(2) }`. CR 122.1.
    #[test]
    fn counters_any_sums_across_counter_types() {
        use crate::types::ability::{Comparator, QuantityExpr, TargetFilter, TypedFilter};
        use crate::types::counter::{CounterMatch, CounterType};
        let mut state = setup();
        let mixed = add_creature(&mut state, PlayerId(0), "Mixed");
        {
            let obj = state.objects.get_mut(&mixed).unwrap();
            obj.counters.insert(CounterType::Stun, 1);
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }
        let ctx = FilterContext::from_source_with_controller(ObjectId(999), PlayerId(0));

        let ge2 =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 2 },
                }]),
            );
        assert!(super::matches_target_filter(&state, mixed, &ge2, &ctx));

        // The comparator axis is honored end-to-end: LE/NE work too.
        let le1 =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::LE,
                    count: QuantityExpr::Fixed { value: 1 },
                }]),
            );
        assert!(!super::matches_target_filter(&state, mixed, &le1, &ctx));

        let ne0 =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Stun),
                    comparator: Comparator::NE,
                    count: QuantityExpr::Fixed { value: 0 },
                }]),
            );
        assert!(super::matches_target_filter(&state, mixed, &ne0, &ctx));
    }

    /// Serde round-trip for `FilterProp::PtComparison.value: QuantityExpr`,
    /// `Counters.count: QuantityExpr`, and `Effect::SearchLibrary.count: QuantityExpr`.
    #[test]
    fn widened_numeric_fields_roundtrip_through_json() {
        use crate::types::ability::{
            Comparator, Effect, PtStat, PtValueScope, QuantityExpr, TargetFilter, TypedFilter,
        };
        use crate::types::counter::{CounterMatch, CounterType};

        let power_filter = FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 3 },
        };
        let json = serde_json::to_string(&power_filter).unwrap();
        let restored: FilterProp = serde_json::from_str(&json).unwrap();
        assert_eq!(power_filter, restored);

        let counters_filter = FilterProp::Counters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            comparator: Comparator::GE,
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
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
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
                exclude_source: crate::types::ability::SourceExclusion::Include,
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
            base_power: Some(2),
            base_toughness: Some(2),
            mana_value: 3,
            controller: PlayerId(1),
            owner: PlayerId(1),
            card_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: Vec::new(),
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
            base_power: None,
            base_toughness: None,
            mana_value: 0,
            controller: PlayerId(1),
            owner: PlayerId(1),
            card_types: vec![CoreType::Land],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: Vec::new(),
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

    /// CR 208.4b + CR 613.4b + CR 603.10a: `FilterProp::PtComparison` on a
    /// zone-change snapshot must honor `scope`. A base-1/1 creature that had a
    /// +1/+1 counter (current 2/2) when it left the battlefield records
    /// `base_power/base_toughness = 1` and `power/toughness = 2`. A look-back
    /// "with base power 1 or less dies" filter (`scope: Base`) must match, while
    /// the same threshold under `scope: Current` must not — proving the
    /// snapshot path reads the captured base value rather than the current one.
    #[test]
    fn zone_change_record_pt_comparison_honors_base_scope() {
        use crate::types::ability::{Comparator, PtStat, PtValueScope, QuantityExpr};
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

        // base 1/1, current 2/2 (had a +1/+1 counter when it left the battlefield).
        let record = ZoneChangeRecord {
            core_types: vec![CoreType::Creature],
            power: Some(2),
            toughness: Some(2),
            base_power: Some(1),
            base_toughness: Some(1),
            ..ZoneChangeRecord::test_minimal(ObjectId(7), Some(Zone::Battlefield), Zone::Graveyard)
        };

        let pt_filter = |stat, scope| FilterProp::PtComparison {
            stat,
            scope,
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 1 },
        };

        // Base scope reads base_power/base_toughness (1) — matches `<= 1`.
        assert!(zone_change_record_matches_property(
            &pt_filter(PtStat::Power, PtValueScope::Base),
            &state,
            &record,
            &source_ctx,
        ));
        assert!(zone_change_record_matches_property(
            &pt_filter(PtStat::Toughness, PtValueScope::Base),
            &state,
            &record,
            &source_ctx,
        ));

        // Current scope reads power/toughness (2) — does NOT match `<= 1`.
        assert!(!zone_change_record_matches_property(
            &pt_filter(PtStat::Power, PtValueScope::Current),
            &state,
            &record,
            &source_ctx,
        ));
        assert!(!zone_change_record_matches_property(
            &pt_filter(PtStat::Toughness, PtValueScope::Current),
            &state,
            &record,
            &source_ctx,
        ));
    }

    /// CR 208.4b + CR 613.4b + CR 603.10a: End-to-end look-back path. Drives a
    /// REAL `GameObject` (base 1/1) with a +1/+1 counter through the live layer
    /// pipeline (`evaluate_layers` makes current power/toughness 2/2 while the
    /// layer-7b base stays 1/1), then snapshots it via the authoritative
    /// production constructor `GameObject::snapshot_for_zone_change` — NOT a
    /// hand-built literal. The resulting `ZoneChangeRecord` must carry
    /// `base_power = 1` (layer-7b base) and `power = 2` (current), and the
    /// snapshot matcher must read the base value under `scope: Base`. This is
    /// the discriminating dies/LTB scenario: "whenever a creature with base
    /// power 1 or less dies" matches, "with power 1 or less" (current) does not.
    #[test]
    fn snapshot_for_zone_change_captures_layer_7b_base_for_lookback_filter() {
        use crate::game::layers::evaluate_layers;
        use crate::types::ability::{Comparator, PtStat, PtValueScope, QuantityExpr};
        use crate::types::counter::CounterType;

        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Base One Creature");
        {
            let obj = state.objects.get_mut(&id).unwrap();
            // Base 1/1 (layer-7b values), plus a +1/+1 counter (layer 7c).
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.base_card_types = obj.card_types.clone();
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }

        // Live layer pass: current becomes 2/2, base stays 1/1.
        evaluate_layers(&mut state);
        {
            let obj = &state.objects[&id];
            assert_eq!(obj.power, Some(2), "counter should raise current power");
            assert_eq!(obj.toughness, Some(2));
            assert_eq!(obj.base_power, Some(1), "layer-7b base power unchanged");
            assert_eq!(obj.base_toughness, Some(1));
        }

        // Authoritative production snapshot constructor (the dies/LTB path).
        let record = state.objects[&id].snapshot_for_zone_change(
            id,
            Some(Zone::Battlefield),
            Zone::Graveyard,
        );
        // The record must carry the layer-7b base, not the current value.
        assert_eq!(
            record.base_power,
            Some(1),
            "snapshot must capture layer-7b base power, not current"
        );
        assert_eq!(record.base_toughness, Some(1));
        assert_eq!(record.power, Some(2), "snapshot must capture current power");
        assert_eq!(record.toughness, Some(2));

        let source_ctx = SourceContext {
            id,
            controller: Some(PlayerId(0)),
            attached_to: None,
            source_is_aura: false,
            source_is_equipment: false,
            chosen_creature_type: None,
            chosen_attributes: &[],
            ability: None,
            recipient_id: None,
        };
        let pt_filter = |scope| FilterProp::PtComparison {
            stat: PtStat::Power,
            scope,
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 1 },
        };

        // Base scope (base 1 <= 1) matches; current scope (current 2 <= 1) does not.
        assert!(
            zone_change_record_matches_property(
                &pt_filter(PtValueScope::Base),
                &state,
                &record,
                &source_ctx,
            ),
            "base power 1 <= 1 must match on the look-back path"
        );
        assert!(
            !zone_change_record_matches_property(
                &pt_filter(PtValueScope::Current),
                &state,
                &record,
                &source_ctx,
            ),
            "current power 2 <= 1 must NOT match — proves base != current on snapshot path"
        );
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
                name: String::new(),
                core_types,
                supertypes,
                subtypes,
                keywords: Vec::new(),
                colors: Vec::new(),
                mana_value: 0,
                has_x_in_cost: false,
                from_zone: Zone::Hand,
                cast_variant: crate::types::game_state::CastingVariant::Normal,
                was_kicked: false,
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
                exclude_source: crate::types::ability::SourceExclusion::Include,
            },
            &state,
            &enchanted_record,
            &source_ctx,
        ));
        assert!(!zone_change_record_matches_property(
            &FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Include,
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
                attacking_alone: true,
                blocking_alone: false,
                defending_player: Some(PlayerId(0)),
            },
            ..ZoneChangeRecord::test_minimal(ObjectId(42), Some(Zone::Battlefield), Zone::Graveyard)
        };
        let blocking_record = ZoneChangeRecord {
            combat_status: ZoneChangeCombatStatus {
                attacking: false,
                blocking: true,
                blocked: false,
                attacking_alone: false,
                blocking_alone: true,
                defending_player: None,
            },
            ..ZoneChangeRecord::test_minimal(ObjectId(43), Some(Zone::Battlefield), Zone::Graveyard)
        };

        assert!(zone_change_record_matches_property(
            &FilterProp::Attacking { defender: None },
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
            &FilterProp::Attacking {
                defender: Some(ControllerRef::You)
            },
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

        // CR 506.5 + CR 603.10a: sole-attacker / sole-blocker look-back reads
        // the captured `attacking_alone` / `blocking_alone` snapshot. The
        // attacking-alone record matches AttackingAlone but not BlockingAlone,
        // and vice versa for the blocking-alone record.
        assert!(zone_change_record_matches_property(
            &FilterProp::AttackingAlone,
            &state,
            &attacking_record,
            &source_ctx,
        ));
        assert!(!zone_change_record_matches_property(
            &FilterProp::BlockingAlone,
            &state,
            &attacking_record,
            &source_ctx,
        ));
        assert!(zone_change_record_matches_property(
            &FilterProp::BlockingAlone,
            &state,
            &blocking_record,
            &source_ctx,
        ));
        assert!(!zone_change_record_matches_property(
            &FilterProp::AttackingAlone,
            &state,
            &blocking_record,
            &source_ctx,
        ));
        // CR 506.5 boundary: a record where the creature attacked but NOT alone
        // (co-attacker present at zone-exit) must fail AttackingAlone.
        let attacked_with_company = ZoneChangeRecord {
            combat_status: ZoneChangeCombatStatus {
                attacking: true,
                blocking: false,
                blocked: false,
                attacking_alone: false,
                blocking_alone: false,
                defending_player: Some(PlayerId(0)),
            },
            ..ZoneChangeRecord::test_minimal(ObjectId(44), Some(Zone::Battlefield), Zone::Graveyard)
        };
        assert!(!zone_change_record_matches_property(
            &FilterProp::AttackingAlone,
            &state,
            &attacked_with_company,
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

    /// CR 400.7 + CR 700.4: A live object can be selected by same-turn zone
    /// history phrases like "cards in your graveyard that were put there from
    /// the battlefield this turn".
    #[test]
    fn zone_changed_this_turn_matches_live_object_history() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let card = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Salvage Target".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state
            .zone_changes_this_turn
            .push(ZoneChangeRecord::test_minimal(
                card,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            ));

        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .controller(ControllerRef::You)
                .properties(vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                    FilterProp::ZoneChangedThisTurn {
                        from: Some(Zone::Battlefield),
                        to: Some(Zone::Graveyard),
                    },
                ]),
        );
        assert!(matches_target_filter(&state, card, &filter, source));

        let wrong_destination =
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(vec![
                FilterProp::ZoneChangedThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Exile),
                },
            ]));
        assert!(!matches_target_filter(
            &state,
            card,
            &wrong_destination,
            source
        ));
    }

    /// CR 702.73a: Stack cascade (Ur-Dragon ReduceCost). Spell-record snapshots
    /// must honour Changeling — `Subtype: Dragon` matches Mistform Ultimus on
    /// the stack via `spell_record_matches_filter`.
    #[test]
    fn changeling_spell_record_matches_subtype_filter() {
        let all_creature_types = vec!["Dragon".to_string(), "Goblin".to_string()];
        let record = SpellCastRecord {
            name: String::new(),
            core_types: vec![CoreType::Creature],
            supertypes: vec![],
            subtypes: vec!["Illusion".to_string()],
            keywords: vec![Keyword::Changeling],
            colors: vec![],
            mana_value: 7,
            has_x_in_cost: false,
            from_zone: Zone::Hand,
            cast_variant: crate::types::game_state::CastingVariant::Normal,
            was_kicked: false,
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
            trigger_definitions: Vec::new(),
            power: Some(2),
            toughness: Some(3),
            base_power: Some(2),
            base_toughness: Some(3),
            colors: vec![],
            mana_value: 5,
            controller: PlayerId(0),
            owner: PlayerId(0),
            from_zone: Some(Zone::Battlefield),
            cast_from_zone: None,
            played_from_zone: None,
            to_zone: Zone::Graveyard,
            attachments: vec![],
            linked_exile_snapshot: vec![],
            is_token: false,
            combat_status: Default::default(),
            co_departed: Vec::new(),
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

    /// Building-block test for the `ParentTarget` effect-context snapshot rung
    /// in `parent_target_shared_quality_values` (CR 608.2k / CR 400.7j): a
    /// `SharesQuality { reference: ParentTarget }` filter must resolve against
    /// the resolving ability's `effect_context_object` LKI snapshot when the
    /// parent effect's referent was an untargeted object never written into
    /// `ability.targets` (e.g. a permanent sacrificed by a parent `Sacrifice`).
    ///
    /// Three sub-cases pin both the `None`-`target_id` and stale-`Some`-
    /// `target_id` paths through the `.or_else` resolution ladder.
    #[test]
    fn parent_target_shares_quality_resolves_via_effect_context_snapshot() {
        use crate::types::ability::{CostPaidObjectSnapshot, SharedQuality, SharedQualityRelation};
        use crate::types::game_state::LKISnapshot;
        use std::collections::HashMap;

        let creature_lki = LKISnapshot {
            name: "Test Creature".to_string(),
            power: Some(2),
            toughness: Some(2),
            base_power: Some(2),
            base_toughness: Some(2),
            mana_value: 2,
            controller: PlayerId(0),
            owner: PlayerId(0),
            card_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: Vec::new(),
            counters: HashMap::new(),
        };
        let land_lki = LKISnapshot {
            name: "Test Land".to_string(),
            power: None,
            toughness: None,
            base_power: None,
            base_toughness: None,
            mana_value: 0,
            controller: PlayerId(0),
            owner: PlayerId(0),
            card_types: vec![CoreType::Land],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: Vec::new(),
            counters: HashMap::new(),
        };

        let filter =
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::SharesQuality {
                    quality: SharedQuality::CardType,
                    reference: Some(Box::new(TargetFilter::ParentTarget)),
                    relation: SharedQualityRelation::Shares,
                }]),
            );

        // Battlefield candidate: a Creature that shares the `Creature` card type.
        let mut state = setup();
        let candidate = add_creature(&mut state, PlayerId(0), "Candidate Creature");
        let source = candidate; // source object only needs to exist.
                                // A `gone` id: never present in `state.objects` and never inserted into
                                // `state.lki_cache` — a stale target id.
        let gone_id = ObjectId(99_999);

        // Sub-case 1: `targets` empty + snapshot's card type matches the candidate.
        let mut ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.effect_context_object = Some(CostPaidObjectSnapshot {
            object_id: gone_id,
            lki: creature_lki.clone(),
        });
        assert!(
            super::matches_target_filter(
                &state,
                candidate,
                &filter,
                &FilterContext::from_ability(&ability),
            ),
            "snapshot Creature shares the Creature card type — must match"
        );

        // Sub-case 2: `targets` empty + snapshot is a Land — no shared card type.
        ability.effect_context_object = Some(CostPaidObjectSnapshot {
            object_id: gone_id,
            lki: land_lki.clone(),
        });
        assert!(
            !super::matches_target_filter(
                &state,
                candidate,
                &filter,
                &FilterContext::from_ability(&ability),
            ),
            "snapshot Land shares no card type with the Creature candidate"
        );

        // Sub-case 3: stale `Some` `target_id` + matching snapshot. The
        // `TargetRef::Object` id resolves to neither a live object nor an
        // `lki_cache` entry, so the snapshot rung must still win.
        let mut stale = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(gone_id)],
            source,
            PlayerId(0),
        );
        stale.effect_context_object = Some(CostPaidObjectSnapshot {
            object_id: gone_id,
            lki: creature_lki.clone(),
        });
        assert!(
            super::matches_target_filter(
                &state,
                candidate,
                &filter,
                &FilterContext::from_ability(&stale),
            ),
            "stale target id must fall through to the effect-context snapshot rung"
        );
    }

    /// CR 608.2h + CR 400.7: A `Typed{controller: ScopedPlayer}` filter
    /// against an exiled object must consult the LKI snapshot for the
    /// at-exile controller, not the live `obj.controller` (which has been
    /// reset to owner per CR 109.4 / CR 400.7 when the object left the
    /// battlefield).
    ///
    /// Scenario: player 0 controlled a creature owned by player 1 (e.g.,
    /// stolen with `Threaten`). The creature is exiled. The live
    /// `obj.controller` resets to owner (player 1). A look-back filter scoped
    /// to player 0 (`ScopedPlayer == 0`) must still match the exiled object
    /// — `effective_controller` reads the LKI's at-exile controller (player 0).
    #[test]
    fn scoped_player_controller_uses_lki_for_exiled_objects() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        // Stolen-then-exiled creature: owner = P1, at-exile controller = P0.
        let stolen = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Stolen Bear".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&stolen).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Post-exile controller reset: per CR 109.4 / CR 400.7, the
            // controller reverts to the owner. The live `obj.controller`
            // is the post-reset value.
            obj.controller = PlayerId(1);
        }
        state.lki_cache.insert(
            stolen,
            LKISnapshot {
                name: "Stolen Bear".to_string(),
                power: Some(2),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 2,
                // The pre-exile (at-departure) controller is P0 — what the
                // look-back filter must read.
                controller: PlayerId(0),
                owner: PlayerId(1),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                counters: Default::default(),
                chosen_attributes: vec![],
            },
        );

        // Filter: creature controlled by ScopedPlayer, with no explicit
        // ability scope set → ScopedPlayer falls back to source_controller
        // (P0). The exiled creature must match because its LKI controller
        // is P0, even though `obj.controller` is now P1 (the owner).
        let filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::ScopedPlayer));
        assert!(
            matches_target_filter(&state, stolen, &filter, source),
            "ScopedPlayer filter must match the exiled creature via LKI \
             (at-exile controller=P0), not the post-exile owner=P1"
        );

        // Sanity: an OpponentRef filter from P0's source must NOT match the
        // same object (because the at-exile controller IS P0 = "you").
        let opp_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
        assert!(
            !matches_target_filter(&state, stolen, &opp_filter, source),
            "Opponent filter must NOT match — at-exile controller is the source's controller"
        );
    }

    /// CR 109.4: Stack objects have controllers, so a stale LKI snapshot must
    /// not override the live spell controller when evaluating controller
    /// filters.
    #[test]
    fn stack_object_controller_uses_live_controller_even_with_lki() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let spell = create_object(
            &mut state,
            CardId(101),
            PlayerId(1),
            "Cast From Exile".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.controller = PlayerId(0);
        }
        state.lki_cache.insert(
            spell,
            LKISnapshot {
                name: "Cast From Exile".to_string(),
                power: None,
                toughness: None,
                base_power: None,
                base_toughness: None,
                mana_value: 2,
                controller: PlayerId(1),
                owner: PlayerId(1),
                card_types: vec![],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                counters: Default::default(),
                chosen_attributes: vec![],
            },
        );

        let filter = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
        assert!(
            matches_target_filter(&state, spell, &filter, source),
            "stack objects have a live controller; stale LKI must not make the spell look opponent-controlled"
        );
    }

    // CR 400.1 + CR 601.2a: a spell-cast record's captured `from_zone` must be
    // honored by `FilterProp::InAnyZone` so "spell you've cast this turn from
    // anywhere other than your hand" counts non-hand casts (the Paradox cycle).
    #[test]
    fn spell_record_in_any_zone_cast_origin() {
        let zones = crate::parser::oracle_target::cast_capable_zones_except(Zone::Hand);
        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InAnyZone {
                zones: zones.clone(),
            }]));
        let controller = PlayerId(0);

        // A graveyard cast (e.g. flashback) is in the "anywhere other than hand" set.
        let from_graveyard = SpellCastRecord {
            from_zone: Zone::Graveyard,
            ..Default::default()
        };
        assert!(
            spell_record_matches_filter(&from_graveyard, &filter, controller, &[]),
            "a spell cast from the graveyard must satisfy InAnyZone[everything except hand]"
        );

        // A normal hand cast is excluded.
        let from_hand = SpellCastRecord {
            from_zone: Zone::Hand,
            ..Default::default()
        };
        assert!(
            !spell_record_matches_filter(&from_hand, &filter, controller, &[]),
            "a spell cast from hand must NOT satisfy InAnyZone[everything except hand]"
        );
    }
}

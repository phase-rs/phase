//! CR 115.7d + CR 115.7e + CR 115.3 + CR 707.10c: completion search for a
//! "choose new targets" walk.
//!
//! A position-by-position choice (a copy's CR 707.10c walk, or any surface that
//! asks which picks remain answerable) must never offer a pick that leaves no
//! legal FINAL target set (CR 115.7e: "only the final set of targets is
//! evaluated"). This module answers, for a decided prefix of picks
//! (`None` = keep, `Some(t)` = choose `t`), whether some completion of the
//! remaining positions is accepted by THE validator,
//! [`engine::validate_retarget_submission`] — never by a second legality model.
//!
//! # Witnesses come only from the validator (W1)
//! The seed is "keep every target" (post == pre), confirmed by the validator
//! like any other vector. Every witness this module returns has been accepted
//! by the validator as a whole vector. Nothing is persisted: each query is
//! re-solved from the confirmed seed.
//!
//! # Components (W3)
//! Positions are coupled only by what the validator evaluates across positions:
//! CR 115.3 distinctness inside a multi-target run, and filters that read
//! another declared slot. [`retarget_dependencies`] classifies every position's
//! filter exhaustively; any target-dependent read it does not name
//! (`SharesQuality`, target-dependent quantities, target-relative
//! controllers/owners, ...) returns [`RetargetDeps::AllPositions`], which joins
//! every position into one combined component. Because "keep everything" is a
//! valid vector and the validator's verdict is the conjunction of per-component
//! verdicts, a component's assignment is checked with every OTHER position
//! kept; the assembled witness is then validated whole, with an exact combined
//! search as the fallback if that ever disagrees.
//!
//! # Solvers (W2')
//! A pure distinctness run whose announced identities are all different is
//! solved by bipartite matching (augmenting paths): there, W2's "no changed
//! pair repeats an identity" equals "the final vector is all-different". Every
//! other component (a run that already repeats an identity, a declared-slot
//! dependency, the combined component) is solved by validator-checked
//! depth-first search, pruned only by fixed pairs that already violate W2.

use std::cell::Cell;

use crate::game::ability_utils::{
    chain_retarget_slots, declared_position_addresses, retarget_final_identities,
    retarget_keep_is_distinct, retarget_positions_changed, SlotEnforcement,
};
use crate::game::engine::{validate_retarget_submission, EngineError, RetargetProposal};
use crate::game::target_occurrences::OccurrenceIdentity;
use crate::types::ability::{
    AttachmentReferent, CombatRelationSubject, ControllerRef, FilterProp, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use crate::types::game_state::{GameState, RetargetScope, RetargetSlotAddress};

/// What a retarget position's legality reads of the OTHER positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetargetDeps {
    /// Reads no other target: decided by its own pool alone.
    Independent,
    /// Reads exactly these declared slots (`AttachedTo { DeclaredTarget }`).
    Slots(Vec<usize>),
    /// Reads the targets in a way the exact solvers do not model; joins the
    /// combined, validator-searched component. Dominates every combination.
    AllPositions,
}

impl RetargetDeps {
    fn or(self, other: RetargetDeps) -> RetargetDeps {
        match (self, other) {
            (RetargetDeps::AllPositions, _) | (_, RetargetDeps::AllPositions) => {
                RetargetDeps::AllPositions
            }
            (RetargetDeps::Independent, other) | (other, RetargetDeps::Independent) => other,
            (RetargetDeps::Slots(mut a), RetargetDeps::Slots(b)) => {
                for slot in b {
                    if !a.contains(&slot) {
                        a.push(slot);
                    }
                }
                RetargetDeps::Slots(a)
            }
        }
    }

    fn all<I: IntoIterator<Item = RetargetDeps>>(deps: I) -> RetargetDeps {
        deps.into_iter()
            .fold(RetargetDeps::Independent, RetargetDeps::or)
    }
}

/// CR 115.7d + CR 608.2b: what `filter` reads of the ability's other targets.
/// Exhaustive with no wildcard on every traversed surface, so a new variant
/// fails the build until it is classified. Types it does not descend into
/// (`QuantityRef`, `PlayerFilter`) are classified `AllPositions`, the
/// fail-closed direction: a misclassified `AllPositions` only costs search
/// time, while a misclassified `Independent` could offer a pick with no legal
/// completion.
pub(crate) fn retarget_dependencies(filter: &TargetFilter) -> RetargetDeps {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_deps(typed),
        TargetFilter::Not { filter } => retarget_dependencies(filter),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            RetargetDeps::all(filters.iter().map(retarget_dependencies))
        }
        TargetFilter::StackAbility { controller, .. } => controller
            .as_ref()
            .map_or(RetargetDeps::Independent, controller_deps),
        TargetFilter::TrackedSetFiltered { filter, .. } => retarget_dependencies(filter),
        TargetFilter::ChosenDamageSource { filter } => filter
            .as_deref()
            .map_or(RetargetDeps::Independent, retarget_dependencies),
        // Untraversed payload: fail closed.
        TargetFilter::PlayerMatching { .. } => RetargetDeps::AllPositions,
        // CR 608.2c: these read the resolving ability's own targets; a declared player
        // names the player an earlier clause announced.
        TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::DeclaredPlayer { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner => RetargetDeps::AllPositions,
        // Read the source, the event, a resolution-time binding or nothing:
        // never another target position.
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::ControllerAndControlledPermanents { .. }
        | TargetFilter::Opponent
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject { .. }
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::LastZoneChanged
        | TargetFilter::CostPaidObject
        | TargetFilter::AmassedArmy
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::EventTargetController
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => RetargetDeps::Independent,
    }
}

fn typed_filter_deps(typed: &TypedFilter) -> RetargetDeps {
    let TypedFilter {
        type_filters,
        controller,
        properties,
    } = typed;
    RetargetDeps::all(
        type_filters
            .iter()
            .map(type_filter_deps)
            .chain(controller.iter().map(controller_deps))
            .chain(properties.iter().map(filter_prop_deps)),
    )
}

fn type_filter_deps(filter: &TypeFilter) -> RetargetDeps {
    match filter {
        TypeFilter::Non(inner) => type_filter_deps(inner),
        TypeFilter::AnyOf(inner) => RetargetDeps::all(inner.iter().map(type_filter_deps)),
        TypeFilter::Creature
        | TypeFilter::Land
        | TypeFilter::Artifact
        | TypeFilter::Enchantment
        | TypeFilter::Instant
        | TypeFilter::Sorcery
        | TypeFilter::Planeswalker
        | TypeFilter::Battle
        | TypeFilter::Kindred
        | TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Any
        | TypeFilter::Subtype(_) => RetargetDeps::Independent,
    }
}

/// CR 115.10: a controller/owner reference that names the ability's target
/// player or a target's controller/owner reads another target position.
fn controller_deps(controller: &ControllerRef) -> RetargetDeps {
    match controller {
        ControllerRef::TargetPlayer
        | ControllerRef::TargetOpponent
        | ControllerRef::DeclaredPlayer { .. }
        | ControllerRef::ParentTargetController
        | ControllerRef::ParentTargetOwner => RetargetDeps::AllPositions,
        ControllerRef::You
        | ControllerRef::Opponent
        | ControllerRef::ScopedPlayer
        | ControllerRef::EventTargetController
        | ControllerRef::DefendingPlayer
        | ControllerRef::ChosenPlayer { .. }
        | ControllerRef::SourceChosenPlayer
        | ControllerRef::TriggeringPlayer
        | ControllerRef::EnchantedPlayer
        | ControllerRef::ActivePlayer
        | ControllerRef::SpecificPlayer { .. } => RetargetDeps::Independent,
    }
}

/// Every quantity READ is in the conservative set: `QuantityRef` is not
/// traversed, so any reference fails closed to `AllPositions`.
fn quantity_deps(quantity: &QuantityExpr) -> RetargetDeps {
    match quantity {
        QuantityExpr::Fixed { .. } => RetargetDeps::Independent,
        QuantityExpr::Ref { .. } => RetargetDeps::AllPositions,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_deps(inner),
        QuantityExpr::UpTo { max } => quantity_deps(max),
        QuantityExpr::Power { exponent, .. } => quantity_deps(exponent),
        QuantityExpr::Difference { left, right } => quantity_deps(left).or(quantity_deps(right)),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            RetargetDeps::all(exprs.iter().map(quantity_deps))
        }
    }
}

fn filter_prop_deps(prop: &FilterProp) -> RetargetDeps {
    match prop {
        FilterProp::AttachedTo { to } => match to {
            AttachmentReferent::DeclaredTarget { slot } => RetargetDeps::Slots(vec![*slot]),
            AttachmentReferent::Player { player } => controller_deps(player),
            AttachmentReferent::Source | AttachmentReferent::Recipient => RetargetDeps::Independent,
        },
        FilterProp::CombatRelation { subject, .. } => match subject {
            CombatRelationSubject::ParentTarget => RetargetDeps::AllPositions,
            CombatRelationSubject::Source => RetargetDeps::Independent,
        },
        FilterProp::Attacking { defender } => defender
            .as_ref()
            .map_or(RetargetDeps::Independent, controller_deps),
        FilterProp::ProtectorMatches { controller } | FilterProp::Owned { controller } => {
            controller_deps(controller)
        }
        FilterProp::HasAttachment { controller, .. } => controller
            .as_ref()
            .map_or(RetargetDeps::Independent, controller_deps),
        FilterProp::CanEnchant { target } => retarget_dependencies(target),
        FilterProp::Counters { count, .. } => quantity_deps(count),
        FilterProp::Cmc { value, .. } => quantity_deps(value),
        FilterProp::ControllerMatches { .. } => RetargetDeps::AllPositions,
        FilterProp::HasAnyAttachmentOf { controller, .. }
        | FilterProp::AttackedThisTurn {
            defender: controller,
        }
        | FilterProp::NameMatchesAnyPermanent { controller } => controller
            .as_ref()
            .map_or(RetargetDeps::Independent, controller_deps),
        FilterProp::MostPrevalentCreatureTypeIn { scope, .. } => controller_deps(scope),
        FilterProp::PtComparison { value, .. } => quantity_deps(value),
        FilterProp::AnyOf { props } => RetargetDeps::all(props.iter().map(filter_prop_deps)),
        FilterProp::Not { prop } => filter_prop_deps(prop),
        FilterProp::DifferentNameFrom { filter }
        | FilterProp::TargetsOnly { filter }
        | FilterProp::Targets { filter } => retarget_dependencies(filter),
        FilterProp::DistinctFrom { reference } => retarget_dependencies(reference),
        // CR 115.7e: a shared-quality or same-name read against another
        // object is not modeled precisely; it routes to the validator-checked
        // combined search (the deferred Daring Thief gap stays with the
        // validator's own domain).
        FilterProp::SharesQuality { .. } | FilterProp::SameNameAsParentTarget => {
            RetargetDeps::AllPositions
        }
        FilterProp::DealtDamageThisTurn { recipient, .. } => match recipient {
            Some(_) => RetargetDeps::AllPositions,
            None => RetargetDeps::Independent,
        },
        // An unclassified fallback property fails closed.
        FilterProp::Other { .. } => RetargetDeps::AllPositions,
        FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::NotColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::MatchesLastChosenCardPredicate
        | FilterProp::HasSingleTarget
        | FilterProp::Modal
        | FilterProp::PrepareSpell
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::Goaded
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InTrackedSet { .. }
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::InAnyZone { .. }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::Transformed
        | FilterProp::CouldBeTargetedByTriggeringSpell
        | FilterProp::HasXInManaCost
        | FilterProp::HasXInActivationCost
        | FilterProp::WasKicked
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsExiledBySource
        | FilterProp::IsCommander
        | FilterProp::SharesCreatureTypeWithCommander => RetargetDeps::Independent,
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::RepresentedByCard
        | FilterProp::ControllerChoseLabel { .. }
        | FilterProp::WasPlayed
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::BlockStatus { .. }
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ConvokedSource
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::WithKeyword { .. }
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeyword { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Foretold
        | FilterProp::HasAdventure
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy => RetargetDeps::Independent,
    }
}

/// One picked value per position: `None` keeps, `Some(t)` chooses `t`.
pub(crate) type RetargetPick = Option<TargetRef>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComponentSolver {
    /// A pure distinctness run with all-different announced identities.
    Matching,
    /// Validator-checked depth-first search.
    Search,
}

#[derive(Debug, Clone)]
struct Component {
    positions: Vec<usize>,
    solver: ComponentSolver,
}

/// The engine-derived answer for one position of a walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OfferedPicks {
    /// The targets that may be chosen here and still complete.
    pub(crate) alternatives: Vec<TargetRef>,
    /// Keeping this position's target still completes.
    pub(crate) can_keep: bool,
}

/// A completion search over one stack entry's addressed retarget positions
/// (`chain_retarget_slots`), against the reducer's own validator.
pub(crate) struct RetargetSearch<'s> {
    state: &'s GameState,
    stack_entry_index: usize,
    pre: &'s ResolvedAbility,
    current_targets: Vec<TargetRef>,
    slots: Vec<RetargetSlotAddress>,
    slot_pools: Vec<Vec<TargetRef>>,
    legal_new_targets: Vec<TargetRef>,
    keep_is_distinct: Vec<bool>,
    runs: Vec<Option<usize>>,
    components: Vec<Component>,
    validator_calls: Cell<usize>,
}

impl<'s> RetargetSearch<'s> {
    /// Build the search for the stack entry at `stack_entry_index`; `None`
    /// when it has no ability or no addressed position.
    pub(crate) fn for_stack_entry(state: &'s GameState, stack_entry_index: usize) -> Option<Self> {
        let entry = state.stack.get(stack_entry_index)?;
        let pre = entry.ability()?;
        let bindings = chain_retarget_slots(pre);
        if bindings.is_empty() {
            return None;
        }
        let slot_pools =
            crate::game::effects::change_targets::derive_slot_pools(state, entry, pre, &bindings);
        let mut legal_new_targets: Vec<TargetRef> = Vec::new();
        for target in slot_pools.iter().flatten() {
            if !legal_new_targets.contains(target) {
                legal_new_targets.push(target.clone());
            }
        }
        let slots: Vec<RetargetSlotAddress> = bindings
            .iter()
            .map(|binding| binding.address.clone())
            .collect();
        let current_targets = bindings
            .iter()
            .map(|binding| binding.current.clone())
            .collect();
        let keep_is_distinct = retarget_keep_is_distinct(state, pre, &slots);
        let runs: Vec<Option<usize>> = bindings.iter().map(|binding| binding.run).collect();

        // CR 115.7e: the coupling graph — runs and declared-slot reads.
        let n = bindings.len();
        let declared = declared_position_addresses(pre);
        let position_of_declared = |slot: usize| -> Option<usize> {
            let address = declared.get(slot)?.as_ref()?;
            slots.iter().position(|candidate| candidate == address)
        };
        let deps: Vec<RetargetDeps> = bindings
            .iter()
            .map(|binding| match &binding.enforcement {
                SlotEnforcement::Filtered(filter) => retarget_dependencies(filter),
                SlotEnforcement::Legacy => RetargetDeps::Independent,
            })
            .collect();
        let combined = deps.contains(&RetargetDeps::AllPositions);
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], x: usize) -> usize {
            let mut root = x;
            while parent[root] != root {
                root = parent[root];
            }
            parent[x] = root;
            root
        }
        let union = |parent: &mut Vec<usize>, a: usize, b: usize| {
            let (ra, rb) = (find(parent, a), find(parent, b));
            if ra != rb {
                parent[ra] = rb;
            }
        };
        let mut dependent = vec![false; n];
        for i in 0..n {
            if combined {
                union(&mut parent, i, 0);
                dependent[i] = true;
                continue;
            }
            for j in 0..i {
                if runs[i].is_some() && runs[i] == runs[j] {
                    union(&mut parent, i, j);
                }
            }
            if let RetargetDeps::Slots(read) = &deps[i] {
                dependent[i] = true;
                for slot in read {
                    if let Some(j) = position_of_declared(*slot) {
                        dependent[j] = true;
                        union(&mut parent, i, j);
                    }
                }
            }
        }
        let mut components: Vec<Component> = Vec::new();
        let mut root_index: Vec<Option<usize>> = vec![None; n];
        for i in 0..n {
            let root = find(&mut parent, i);
            let k = *root_index[root].get_or_insert_with(|| {
                components.push(Component {
                    positions: Vec::new(),
                    solver: ComponentSolver::Search,
                });
                components.len() - 1
            });
            components[k].positions.push(i);
        }
        let mut search = RetargetSearch {
            state,
            stack_entry_index,
            pre,
            current_targets,
            slots,
            slot_pools,
            legal_new_targets,
            keep_is_distinct,
            runs,
            components,
            validator_calls: Cell::new(0),
        };
        for k in 0..search.components.len() {
            let positions = &search.components[k].positions;
            let pure_run = positions.len() > 1
                && positions.iter().all(|&i| !dependent[i])
                && positions.iter().all(|&i| {
                    search.runs[i].is_some() && search.runs[i] == search.runs[positions[0]]
                });
            let announced: Vec<OccurrenceIdentity> = positions
                .iter()
                .map(|&i| search.identity(i, &None))
                .collect();
            let all_different = announced
                .iter()
                .enumerate()
                .all(|(a, x)| announced[..a].iter().all(|y| !x.same_object(y)));
            search.components[k].solver = if pure_run && all_different {
                ComponentSolver::Matching
            } else {
                ComponentSolver::Search
            };
        }
        Some(search)
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn slots(&self) -> &[RetargetSlotAddress] {
        &self.slots
    }

    pub(crate) fn current_targets(&self) -> &[TargetRef] {
        &self.current_targets
    }

    pub(crate) fn slot_pools(&self) -> &[Vec<TargetRef>] {
        &self.slot_pools
    }

    /// The number of whole-vector validator calls made so far (measurement).
    #[cfg(test)]
    pub(crate) fn validator_calls(&self) -> usize {
        self.validator_calls.get()
    }

    /// The number of components, and how many use matching (measurement).
    #[cfg(test)]
    pub(crate) fn component_census(&self) -> (usize, usize) {
        (
            self.components.len(),
            self.components
                .iter()
                .filter(|component| component.solver == ComponentSolver::Matching)
                .count(),
        )
    }

    /// THE validator, on a whole pick vector.
    pub(crate) fn validate(&self, picks: &[RetargetPick]) -> Result<ResolvedAbility, EngineError> {
        self.validator_calls.set(self.validator_calls.get() + 1);
        validate_retarget_submission(
            self.state,
            &RetargetProposal {
                stack_entry_index: self.stack_entry_index,
                scope: &RetargetScope::All,
                current_targets: &self.current_targets,
                slots: &self.slots,
                slot_pools: &self.slot_pools,
                legal_new_targets: &self.legal_new_targets,
                new_targets: picks,
            },
        )
    }

    /// W1: the seed — keep every target — confirmed by the validator.
    pub(crate) fn confirm_seed(&self) -> Result<(), EngineError> {
        self.validate(&vec![None; self.len()]).map(|_| ())
    }

    /// The picks a position can take: keep, then each pool member, skipping a
    /// re-choice of the announced object that is not a distinct election.
    fn candidates(&self, position: usize) -> Vec<RetargetPick> {
        let mut out = vec![None];
        for target in self.slot_pools.get(position).into_iter().flatten() {
            if self.current_targets.get(position) == Some(target)
                && !self
                    .keep_is_distinct
                    .get(position)
                    .copied()
                    .unwrap_or(false)
            {
                continue;
            }
            out.push(Some(target.clone()));
        }
        out
    }

    fn changed(&self, position: usize, pick: &RetargetPick) -> bool {
        retarget_positions_changed(
            self.state,
            self.pre,
            std::slice::from_ref(&self.slots[position]),
            std::slice::from_ref(pick),
        )
        .first()
        .copied()
        .unwrap_or(false)
    }

    fn identity(&self, position: usize, pick: &RetargetPick) -> OccurrenceIdentity {
        let changed = self.changed(position, pick);
        retarget_final_identities(
            self.state,
            self.pre,
            std::slice::from_ref(&self.slots[position]),
            std::slice::from_ref(pick),
            &[changed],
        )
        .pop()
        .expect("one identity per position")
    }

    /// W2 (CR 115.3): whether two positions' picks are an admissible pair —
    /// different runs, both unchanged, or different objects.
    fn pair_admissible(
        &self,
        a: usize,
        pick_a: &RetargetPick,
        b: usize,
        pick_b: &RetargetPick,
    ) -> bool {
        if self.runs[a].is_none() || self.runs[a] != self.runs[b] {
            return true;
        }
        if !self.changed(a, pick_a) && !self.changed(b, pick_b) {
            return true;
        }
        !self
            .identity(a, pick_a)
            .same_object(&self.identity(b, pick_b))
    }

    /// Validate `assignment` with every other position kept.
    fn locally_accepted(&self, assignment: &[(usize, RetargetPick)]) -> bool {
        let mut picks = vec![None; self.len()];
        for (position, pick) in assignment {
            picks[*position] = pick.clone();
        }
        self.validate(&picks).is_ok()
    }

    fn solve_component(
        &self,
        k: usize,
        fixed: &[Option<RetargetPick>],
    ) -> Option<Vec<(usize, RetargetPick)>> {
        let component = &self.components[k];
        if component.solver == ComponentSolver::Matching {
            // A pure distinctness run with all-different announced identities:
            // every legal completion is an all-different matching, so a failed
            // matching is conclusive and never escalates to search. Only a
            // matching the validator refuses locally falls back to search.
            let assignment = self.solve_matching(&component.positions, fixed)?;
            if self.locally_accepted(&assignment) {
                return Some(assignment);
            }
        }
        self.solve_search(&component.positions, fixed)
    }

    /// Augmenting-path bipartite matching of positions to identities.
    fn solve_matching(
        &self,
        positions: &[usize],
        fixed: &[Option<RetargetPick>],
    ) -> Option<Vec<(usize, RetargetPick)>> {
        let options: Vec<Vec<(RetargetPick, OccurrenceIdentity)>> = positions
            .iter()
            .map(|&position| {
                let picks = match fixed.get(position).cloned().flatten() {
                    Some(pick) => vec![pick],
                    None => self.candidates(position),
                };
                picks
                    .into_iter()
                    .map(|pick| {
                        let identity = self.identity(position, &pick);
                        (pick, identity)
                    })
                    .collect()
            })
            .collect();
        let mut keys: Vec<OccurrenceIdentity> = Vec::new();
        for option in options.iter().flatten() {
            if !keys.iter().any(|key| key.same_object(&option.1)) {
                keys.push(option.1.clone());
            }
        }
        let key_of = |identity: &OccurrenceIdentity| {
            keys.iter()
                .position(|key| key.same_object(identity))
                .expect("every identity has a key")
        };
        let edges: Vec<Vec<(usize, usize)>> = options
            .iter()
            .map(|option| {
                option
                    .iter()
                    .enumerate()
                    .map(|(choice, (_, identity))| (key_of(identity), choice))
                    .collect()
            })
            .collect();
        let mut owner: Vec<Option<(usize, usize)>> = vec![None; keys.len()];
        fn augment(
            left: usize,
            edges: &[Vec<(usize, usize)>],
            owner: &mut [Option<(usize, usize)>],
            seen: &mut [bool],
        ) -> bool {
            for &(key, choice) in &edges[left] {
                if seen[key] {
                    continue;
                }
                seen[key] = true;
                let free = match owner[key] {
                    None => true,
                    Some((other, _)) => augment(other, edges, owner, seen),
                };
                if free {
                    owner[key] = Some((left, choice));
                    return true;
                }
            }
            false
        }
        for left in 0..positions.len() {
            let mut seen = vec![false; keys.len()];
            if !augment(left, &edges, &mut owner, &mut seen) {
                return None;
            }
        }
        let mut assignment: Vec<(usize, RetargetPick)> = owner
            .iter()
            .flatten()
            .map(|&(left, choice)| (positions[left], options[left][choice].0.clone()))
            .collect();
        assignment.sort_by_key(|(position, _)| *position);
        Some(assignment)
    }

    /// Validator-checked depth-first search over the undecided positions.
    fn solve_search(
        &self,
        positions: &[usize],
        fixed: &[Option<RetargetPick>],
    ) -> Option<Vec<(usize, RetargetPick)>> {
        let mut assignment: Vec<(usize, RetargetPick)> = positions
            .iter()
            .filter_map(|&position| {
                fixed
                    .get(position)
                    .cloned()
                    .flatten()
                    .map(|pick| (position, pick))
            })
            .collect();
        let open: Vec<usize> = positions
            .iter()
            .copied()
            .filter(|&position| fixed.get(position).cloned().flatten().is_none())
            .collect();
        if self.search(&open, &mut assignment) {
            assignment.sort_by_key(|(position, _)| *position);
            Some(assignment)
        } else {
            None
        }
    }

    fn search(&self, open: &[usize], assignment: &mut Vec<(usize, RetargetPick)>) -> bool {
        let Some((&position, rest)) = open.split_first() else {
            return self.locally_accepted(assignment);
        };
        for pick in self.candidates(position) {
            let admissible = assignment.iter().all(|(other, other_pick)| {
                self.pair_admissible(position, &pick, *other, other_pick)
            });
            if !admissible {
                continue;
            }
            assignment.push((position, pick));
            if self.search(rest, assignment) {
                return true;
            }
            assignment.pop();
        }
        false
    }

    /// CR 115.7e: a complete pick vector extending `prefix` that the validator
    /// accepts, or `None` when none exists.
    pub(crate) fn completion(&self, prefix: &[RetargetPick]) -> Option<Vec<RetargetPick>> {
        // W2 (CR 115.3): a decided prefix that already names one object twice
        // inside a run (with a changed position) has no completion; no search
        // over the open positions can repair a fixed pair.
        let decided = &prefix[..prefix.len().min(self.len())];
        for (a, pick_a) in decided.iter().enumerate() {
            for (b, pick_b) in decided[..a].iter().enumerate() {
                if !self.pair_admissible(a, pick_a, b, pick_b) {
                    return None;
                }
            }
        }
        let mut fixed: Vec<Option<RetargetPick>> = vec![None; self.len()];
        for (position, pick) in prefix.iter().enumerate().take(self.len()) {
            fixed[position] = Some(pick.clone());
        }
        let mut picks: Vec<RetargetPick> = vec![None; self.len()];
        for k in 0..self.components.len() {
            for (position, pick) in self.solve_component(k, &fixed)? {
                picks[position] = pick;
            }
        }
        if self.validate(&picks).is_ok() {
            return Some(picks);
        }
        // Exact combined fallback: one search over every open position.
        let all: Vec<usize> = (0..self.len()).collect();
        let assignment = self.solve_search_whole(&all, &fixed)?;
        let mut picks: Vec<RetargetPick> = vec![None; self.len()];
        for (position, pick) in assignment {
            picks[position] = pick;
        }
        Some(picks)
    }

    fn solve_search_whole(
        &self,
        positions: &[usize],
        fixed: &[Option<RetargetPick>],
    ) -> Option<Vec<(usize, RetargetPick)>> {
        // `locally_accepted` keeps every unassigned position, so with every
        // position assigned it is the whole-vector validator.
        self.solve_search(positions, fixed)
    }

    /// The picks offered at position `prefix.len()`.
    pub(crate) fn offered(&self, prefix: &[RetargetPick]) -> OfferedPicks {
        let position = prefix.len();
        if position >= self.len() {
            return OfferedPicks::default();
        }
        let mut offered = OfferedPicks::default();
        for pick in self.candidates(position) {
            let mut extended = prefix.to_vec();
            extended.push(pick.clone());
            if self.completion(&extended).is_some() {
                match pick {
                    None => offered.can_keep = true,
                    Some(target) => offered.alternatives.push(target),
                }
            }
        }
        offered
    }

    /// CR 707.10c: keeping every remaining target completes.
    pub(crate) fn can_keep_rest(&self, prefix: &[RetargetPick]) -> bool {
        let mut picks = prefix.to_vec();
        picks.resize(self.len(), None);
        self.validate(&picks).is_ok()
    }

    /// Whether `pick` at position `prefix.len()` is answerable.
    pub(crate) fn admits(&self, prefix: &[RetargetPick], pick: &RetargetPick) -> bool {
        let mut extended = prefix.to_vec();
        extended.push(pick.clone());
        self.completion(&extended).is_some()
    }
}

#[cfg(test)]
#[path = "retarget_completion_tests.rs"]
mod tests;

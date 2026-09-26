//! CR 608.2c + CR 405.5: which objects and players each node of a pending stack
//! entry will act on when that entry resolves.

use std::borrow::Cow;

use crate::game::ability_utils::{
    apply_instead_swap, flatten_specified_targets_in_chain, validate_targets_in_chain,
};
use crate::game::functioning_abilities::active_replacements;
use crate::game::replacement::{find_applicable_replacements, replacement_registry};
use crate::game::{stack, targeting};
use crate::types::ability::{
    AbilityCondition, AbilityCost, Duration, Effect, EffectOutcomeSignal, EffectScope, ObjectScope,
    ResolvedAbility, TapStateChange, TargetFilter, TargetRef,
};
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// One node of a stack entry's chain and the objects and players its resolver
/// acts on.
#[derive(Debug)]
pub struct NodeReach<'a> {
    /// The node as it sits on the stack.
    pub node: &'a ResolvedAbility,
    /// What the node's resolver acts on (see [`stack_entry_node_reach`]).
    pub acted_on: Vec<TargetRef>,
}

/// CR 608.2c: each node of `entry`'s chain (the node, then its `sub_ability`
/// subtree, then its `else_ability` subtree), paired with the objects and
/// players its resolver will act on: its reach, assuming optional choices are
/// taken. A "you may", an unless cost, and an "if you do" after a "you may"
/// are answered as if the node is performed. The answer names what a node's
/// instruction is applied to; whether the instruction changes it is not
/// predicted.
///
/// Each node is answered on the copy of the board `resolution_board` builds,
/// as the entry stands before its first instruction runs. A node below an
/// instruction that may change what the node's answer reads answers nothing
/// (`reads_what_was_written`). A node below an instruction whose changes this
/// authority cannot bound (`instruction_writes`) answers nothing, and so does a
/// node that repeats, resolves once per chosen player, or waits on a condition
/// only its resolution decides; every node below such a node answers nothing
/// too.
///
/// CR 405.5: a stack object above `entry` resolves first, so it is never in the
/// answer.
///
/// `pub` so `phase-ai`'s stack readers ask the engine which objects a pending
/// node acts on instead of re-deriving the resolvers' binding.
pub fn stack_entry_node_reach<'a>(state: &GameState, entry: &'a StackEntry) -> Vec<NodeReach<'a>> {
    let Some(root) = entry.ability() else {
        return Vec::new();
    };
    let resolves_first: Vec<ObjectId> = state
        .stack
        .iter()
        .position(|e| e.id == entry.id)
        .map(|index| state.stack.iter().skip(index + 1).map(|e| e.id).collect())
        .unwrap_or_default();
    let (board, prepared) = match resolution_board(state, entry, root) {
        Some((board, prepared)) => (Some(board), Cow::Owned(prepared)),
        None => (None, Cow::Borrowed(root)),
    };
    let walk = Walk {
        root: &prepared,
        resolves_first: &resolves_first,
    };
    let mut reaches = Vec::new();
    walk.push_node(
        board.as_ref(),
        root,
        &prepared,
        Cow::Borrowed(&prepared),
        &Before::default(),
        &mut reaches,
    );
    reaches
}

/// A copy of `state` as `stack::resolve_top` leaves it just before `entry`'s
/// first instruction runs, paired with `root` as `resolve_top` rewrites it
/// (`stack::bind_resolving_ability_referents`). On the copy `entry` is the
/// resolving entry, the one `targeting::resolving_root_ability` reads a
/// `ParentTargetSlot` from, and its resolution scope is bound by
/// `stack::bind_resolution_scope`, so a trigger's "that player" or "that
/// spell" reads this entry's own trigger event and event batch.
///
/// `None` when `resolve_top` would not run the chain's instructions as they
/// stand: a CR 603.4 intervening-if that is false, a spell whose control
/// changed on the stack (CR 608.2c + CR 109.5: `resolve_top` re-stamps its
/// controller), a required target that has not been chosen
/// (`stack::has_missing_required_stack_targets`), or a CR 608.2b declared
/// target that is no longer legal.
fn resolution_board(
    state: &GameState,
    entry: &StackEntry,
    root: &ResolvedAbility,
) -> Option<(GameState, ResolvedAbility)> {
    if matches!(entry.kind, StackEntryKind::Spell { .. })
        && stack::stack_object_controller(state, entry) != root.controller
    {
        return None;
    }
    let mut board = state.clone();
    let batch = board.stack_trigger_event_batches.remove(&entry.id);
    // `resolve_top` clears the trigger event when a resolution completes, so
    // one in progress now is not `entry`'s.
    board.current_trigger_event = None;
    board.current_trigger_events.clear();
    // `resolve_top` clears the self-move re-latch before a resolution begins, so
    // one set now is another resolution's.
    board.resolution_source_relatch = None;
    board.resolving_stack_entry = Some(entry.clone());
    if !stack::bind_resolution_scope(&mut board, entry, batch) {
        return None;
    }
    let mut prepared = root.clone();
    stack::bind_resolving_ability_referents(&board, entry, &mut prepared);
    if stack::has_missing_required_stack_targets(&board, &prepared) {
        return None;
    }
    let declared = flatten_specified_targets_in_chain(&prepared);
    let legal = flatten_specified_targets_in_chain(&validate_targets_in_chain(&board, &prepared));
    if legal != declared {
        return None;
    }
    super::reset_top_level_resolution_state(&mut board);
    super::count_top_level_resolution(&mut board, &prepared);
    Some((board, prepared))
}

/// Where a node sits among the instructions its entry runs.
#[derive(Default)]
struct Before {
    /// Whether any instruction of the entry runs before the node.
    later: bool,
    /// The instruction the node follows is a "you may".
    after_optional: bool,
    /// What the instructions that run before the node may have changed.
    written: Written,
}

/// CR 608.2c + CR 608.2h: what the instructions an entry runs before a node
/// may have changed, for instructions whose changes `instruction_writes`
/// bounds.
#[derive(Clone, Default)]
struct Written {
    /// Some instruction changed the game.
    anything: bool,
    /// Zones an instruction may move cards out of.
    moved_from: Vec<Zone>,
    /// The events that replacement effects an instruction created replace.
    replacements: Vec<ReplacementEvent>,
}

impl Written {
    fn changed() -> Self {
        Written {
            anything: true,
            ..Written::default()
        }
    }

    fn and(mut self, other: Written) -> Self {
        self.anything |= other.anything;
        self.moved_from.extend(other.moved_from);
        self.replacements.extend(other.replacements);
        self
    }
}

struct Walk<'w> {
    /// The root as `resolution_board` rewrote it. The chain resolver copies its
    /// `context` to every later instruction (`apply_parent_chain_context`).
    root: &'w ResolvedAbility,
    resolves_first: &'w [ObjectId],
}

/// Whether a node's own instruction runs.
enum Runs {
    Yes,
    /// Its effect is skipped and the nodes below it run as they would.
    NotItself,
    /// It does not run; the nodes below it are not answered.
    No,
}

impl Walk<'_> {
    /// `prepared` is `node` as `resolution_board` rewrote it, and `bound` is
    /// `prepared` holding the targets it resolves with: its own, or the ones its
    /// parent hands it.
    fn push_node<'a>(
        &self,
        board: Option<&GameState>,
        node: &'a ResolvedAbility,
        prepared: &ResolvedAbility,
        bound: Cow<'_, ResolvedAbility>,
        before: &Before,
        reaches: &mut Vec<NodeReach<'a>>,
    ) {
        let board = board.filter(|_| resolves_once_as_bound(&bound));
        // CR 608.2c: an "instead" node replaces this node's effect when its
        // condition holds, and is consumed either way.
        let instead = node
            .sub_ability
            .as_deref()
            .zip(prepared.sub_ability.as_deref())
            .filter(|(_, sub)| is_instead_override(sub));
        let swap = match (board, instead) {
            (Some(board), Some((_, sub))) => self.instead_swap(board, &bound, sub, before),
            _ => Some(false),
        };
        let swapped = swap == Some(true);
        let performed = match instead.filter(|_| swapped) {
            Some((_, sub)) => Cow::Owned(apply_instead_swap(&bound, sub)),
            None => Cow::Borrowed(bound.as_ref()),
        };
        let board = board.filter(|_| swap.is_some() && resolves_once_as_bound(&performed));
        let runs = board.map_or(Runs::No, |board| self.runs(board, &performed, before));
        let acted_on = match (board, &runs) {
            (Some(board), Runs::Yes) => {
                let declared = if swapped {
                    performed.as_ref()
                } else {
                    prepared
                };
                let acted_on = self.acted_on(board, declared, &performed);
                if reads_what_was_written(board, &performed, &acted_on, &before.written) {
                    Vec::new()
                } else {
                    acted_on
                }
            }
            _ => Vec::new(),
        };
        let written = board.and_then(|board| {
            instruction_writes(board, &performed, &acted_on, &before.written)
                .map(|own| before.written.clone().and(own))
        });
        let board = board.filter(|_| !matches!(runs, Runs::No));
        let (own, instead_answer) = if swapped {
            (Vec::new(), acted_on)
        } else {
            (acted_on, Vec::new())
        };
        reaches.push(NodeReach {
            node,
            acted_on: own,
        });
        let children = [
            (node.sub_ability.as_deref(), prepared.sub_ability.as_deref()),
            (
                node.else_ability.as_deref(),
                prepared.else_ability.as_deref(),
            ),
        ];
        for (index, (child, prepared_child)) in children.into_iter().enumerate() {
            debug_assert_eq!(
                child.is_some(),
                prepared_child.is_some(),
                "the resolution rewrites keep the chain's shape"
            );
            let (Some(child), Some(prepared_child)) = (child, prepared_child) else {
                continue;
            };
            if index == 0 && instead.is_some() {
                // What runs in this node's place when the swap happened. The
                // nodes below it are not answered.
                reaches.push(NodeReach {
                    node: child,
                    acted_on: instead_answer.clone(),
                });
                push_unanswered_below(child, reaches);
                continue;
            }
            // An `else_ability` runs when this node's condition is false or its
            // "you may" is declined, and neither is answered.
            let handed = board
                .filter(|_| index == 0)
                .zip(written.clone())
                .zip(handed_child(&performed, prepared_child));
            let Some(((child_board, written), child_bound)) = handed else {
                reaches.push(NodeReach {
                    node: child,
                    acted_on: Vec::new(),
                });
                push_unanswered_below(child, reaches);
                continue;
            };
            self.push_node(
                Some(child_board),
                child,
                prepared_child,
                child_bound,
                &Before {
                    later: true,
                    after_optional: performed.optional,
                    written,
                },
                reaches,
            );
        }
    }

    /// CR 608.2c: whether `sub`, an "instead" node, replaces `bound`'s effect.
    /// `None` below the entry's first instruction, where this authority does not
    /// answer an "instead" node.
    fn instead_swap(
        &self,
        board: &GameState,
        bound: &ResolvedAbility,
        sub: &ResolvedAbility,
        before: &Before,
    ) -> Option<bool> {
        (!before.later).then(|| super::instead_swap_applies(board, bound, sub))
    }

    /// The checks the chain resolver makes before `ability`'s instruction
    /// runs, in its order.
    fn runs(&self, board: &GameState, ability: &ResolvedAbility, before: &Before) -> Runs {
        if let Some(condition) = &ability.condition {
            if self.condition_holds(board, condition, ability, before) != Some(true) {
                return Runs::No;
            }
        }
        // The chain resolver treats an unless cost of {0} as paid, so a counter
        // with one counters nothing and the rest of the entry does not run.
        // Where no player would be asked, it counters; that answer is nothing
        // here too.
        if let Some(unless_pay) = &ability.unless_pay {
            if matches!(ability.effect, Effect::Counter { .. })
                && matches!(
                    super::resolved_unless_cost(board, ability, &unless_pay.cost),
                    AbilityCost::Mana { cost } if cost == ManaCost::zero()
                )
            {
                return Runs::No;
            }
        }
        if super::fails_shared_quality(board, ability) {
            return Runs::NotItself;
        }
        Runs::Yes
    }

    /// CR 608.2c: `condition` as the chain resolver evaluates it for `ability`,
    /// or `None` where it reads something an earlier instruction of the entry
    /// can change.
    fn condition_holds(
        &self,
        board: &GameState,
        condition: &AbilityCondition,
        ability: &ResolvedAbility,
        before: &Before,
    ) -> Option<bool> {
        if !before.later {
            return Some(super::evaluate_condition(condition, board, ability));
        }
        if before.after_optional
            && matches!(
                condition,
                AbilityCondition::EffectOutcome {
                    signal: EffectOutcomeSignal::OptionalEffectPerformed,
                }
            )
        {
            // "If you do" after a "you may": answered as if the player does.
            return Some(true);
        }
        is_fixed_before_resolution(condition)
            .then(|| super::evaluate_condition(condition, board, &self.with_root_context(ability)))
    }

    /// `ability` holding the root's context, as the chain resolver hands it to
    /// every later instruction.
    fn with_root_context(&self, ability: &ResolvedAbility) -> ResolvedAbility {
        let mut owned = ability.clone();
        owned.context = self.root.context.clone();
        owned
    }

    /// What `bound`'s resolver acts on, or nothing where this authority cannot
    /// say exactly.
    fn acted_on(
        &self,
        board: &GameState,
        declared: &ResolvedAbility,
        bound: &ResolvedAbility,
    ) -> Vec<TargetRef> {
        if names_resolution_product(&bound.effect) || names_unbound_referent(board, bound) {
            return Vec::new();
        }
        let acted_on: Vec<TargetRef> = node_acted_on(board, declared, bound)
            .into_iter()
            .filter(|target| {
                !matches!(target, TargetRef::Object(id) if self.resolves_first.contains(id))
            })
            .collect();
        if replacement_may_apply(board, bound, &acted_on) {
            return Vec::new();
        }
        acted_on
    }
}

/// `node`'s subtree below it, every node answering nothing.
fn push_unanswered_below<'a>(node: &'a ResolvedAbility, reaches: &mut Vec<NodeReach<'a>>) {
    for child in [node.sub_ability.as_deref(), node.else_ability.as_deref()]
        .into_iter()
        .flatten()
    {
        reaches.push(NodeReach {
            node: child,
            acted_on: Vec::new(),
        });
        push_unanswered_below(child, reaches);
    }
}

/// A node that acts once, on targets fixed before its entry resolves. For any
/// other node the node and every node below it answer nothing:
/// - the chain resolver repeats a `player_scope` node once for each player in
///   the scope, with that player acting;
/// - it resolves a node over "any number of target players" once per chosen
///   player;
/// - it repeats a `repeat_for` node, rebinding its referent per member where
///   the count is a population.
fn resolves_once_as_bound(bound: &ResolvedAbility) -> bool {
    bound.player_scope.is_none()
        && !super::resolves_for_each_target_player(bound)
        && bound.repeat_for.is_none()
}

/// CR 608.2c: a node that replaces its parent's effect when its condition
/// holds.
fn is_instead_override(node: &ResolvedAbility) -> bool {
    matches!(
        node.condition,
        Some(
            AbilityCondition::AdditionalCostPaidInstead
                | AbilityCondition::CastVariantPaidInstead { .. }
                | AbilityCondition::TargetHasKeywordInstead { .. }
                | AbilityCondition::ConditionInstead { .. }
        )
    )
}

/// A condition that reads only what the entry's context recorded when it was
/// cast: the costs paid, the zone and the phase it was cast from. The chain
/// resolver copies that context to every later instruction, and no
/// instruction of the entry changes it.
fn is_fixed_before_resolution(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::AdditionalCostPaid { subject, .. } => matches!(
            subject,
            ObjectScope::Source | ObjectScope::Anaphoric | ObjectScope::Demonstrative
        ),
        AbilityCondition::AlternativeManaCostPaid
        | AbilityCondition::WasCast { .. }
        | AbilityCondition::CastDuringPhase { .. } => true,
        AbilityCondition::Not { condition } => is_fixed_before_resolution(condition),
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().all(is_fixed_before_resolution)
        }
        _ => false,
    }
}

/// CR 608.2c + CR 608.2h: a later instruction reads the game as the earlier
/// ones left it. What `ability`'s resolution may change, or `None` where this
/// authority cannot bound it, and then no node below it is answered.
/// `acted_on` is its answer and `before` what the instructions before it
/// changed.
///
/// NOTE: the `_` arm answers `None`. An instruction given an arm here must
/// change nothing `names_only_what_it_holds` says its resolvers read, beyond
/// what the returned `Written` records.
fn instruction_writes(
    board: &GameState,
    ability: &ResolvedAbility,
    acted_on: &[TargetRef],
    before: &Written,
) -> Option<Written> {
    match &ability.effect {
        // The chain resolver's `NoOp` arm only reports that it resolved.
        Effect::NoOp => Some(Written::default()),
        // Its `TargetOnly` arm only holds the targets chosen for it, unless it
        // holds an object under a scoped player, which it publishes to the
        // chain's tracked set.
        Effect::TargetOnly { .. } => (ability.scoped_player.is_none()
            || !ability
                .targets
                .iter()
                .any(|target| matches!(target, TargetRef::Object(_))))
        .then(Written::default),
        // CR 613.4c: a pump changes the power and toughness of what it acts
        // on.
        Effect::Pump { .. } => Some(Written::changed()),
        // CR 120.3: damage marks damage, adds or removes counters and changes
        // life totals. A damage node is answered only where
        // `replacement_may_apply` finds no replacement effect for its damage.
        Effect::DealDamage { .. } if !acted_on.is_empty() => Some(Written::changed()),
        // CR 701.19a: regeneration creates a replacement effect for its
        // permanent's next destruction.
        Effect::Regenerate { .. } => Some(Written {
            replacements: vec![ReplacementEvent::Destroy],
            ..Written::changed()
        }),
        // CR 701.25a: surveil moves cards from its player's library to their
        // graveyard, where no replacement effect may apply to that move.
        Effect::Surveil { target, .. } => {
            let player = super::resolve_player_for_context_ref(board, ability, target);
            let moves: Vec<ProposedEvent> = board
                .players
                .iter()
                .filter(|candidate| candidate.id == player)
                .flat_map(|player| player.library.iter())
                .map(|id| {
                    ProposedEvent::zone_change(
                        *id,
                        Zone::Library,
                        Zone::Graveyard,
                        Some(ability.source_id),
                    )
                })
                .collect();
            let replaced = moves.iter().any(|event| {
                !find_applicable_replacements(board, event, replacement_registry()).is_empty()
            }) || replacement_may_come_to_apply(board, &moves, before);
            (!replaced).then(|| Written {
                moved_from: vec![Zone::Library],
                ..Written::changed()
            })
        }
        _ => None,
    }
}

/// CR 608.2c + CR 608.2h: whether an instruction before `bound` may have
/// changed what its answer `acted_on` reads. Any change may, unless
/// `names_only_what_it_holds` accepts `bound`; then only a move of an object it
/// names, or a replacement effect that may come to apply to an event its
/// resolver proposes.
fn reads_what_was_written(
    board: &GameState,
    bound: &ResolvedAbility,
    acted_on: &[TargetRef],
    written: &Written,
) -> bool {
    if !written.anything {
        return false;
    }
    if !names_only_what_it_holds(bound) {
        return true;
    }
    let moved = acted_on
        .iter()
        .chain(&bound.targets)
        .any(|target| match target {
            TargetRef::Object(id) => board
                .objects
                .get(id)
                .is_none_or(|object| written.moved_from.contains(&object.zone)),
            TargetRef::Player(_) => false,
        });
    moved || replacement_may_come_to_apply(board, &proposed_events(board, bound, acted_on), written)
}

/// A node whose resolver chooses what it acts on from referents fixed before
/// its entry resolves, such as its entry's targets and root slots, its source,
/// its trigger event and its controller. A node whose targets must share a
/// quality (`fails_shared_quality`) reads their characteristics.
fn names_only_what_it_holds(bound: &ResolvedAbility) -> bool {
    super::effect_target_filter(&bound.effect)
        .is_none_or(|filter| super::extract_shares_quality_props(filter).is_empty())
        && matches!(
            bound.effect,
            Effect::Counter { .. }
                | Effect::Fight { .. }
                | Effect::DealDamage { .. }
                | Effect::Destroy { .. }
                | Effect::LoseLife { .. }
        )
}

/// CR 614.1: whether a replacement effect may come to apply to one of
/// `events` once the instructions `written` records have run: one they
/// created, or any replacement effect for that kind of event that an object
/// carries or the game holds, whatever its condition reads now.
fn replacement_may_come_to_apply(
    board: &GameState,
    events: &[ProposedEvent],
    written: &Written,
) -> bool {
    if !written.anything {
        return false;
    }
    let registry = replacement_registry();
    let replaces = |kind: &ReplacementEvent, host: ObjectId, event: &ProposedEvent| {
        registry
            .get(kind)
            .is_some_and(|handler| (handler.matcher)(event, host, board))
    };
    events.iter().any(|event| {
        // `instruction_writes` records only regeneration shields, each of
        // which replaces an event on the permanent it protects.
        written.replacements.iter().any(|kind| {
            event
                .affected_object_id()
                .is_some_and(|host| replaces(kind, host, event))
        }) || active_replacements(board)
            .any(|(_, object, definition)| replaces(&definition.event, object.id, event))
            || board.pending_damage_replacements.iter().any(|definition| {
                !definition.is_consumed && replaces(&definition.event, ObjectId(0), event)
            })
    })
}

/// `child` holding the targets it resolves with when the chain resolver reaches
/// it after `parent`, or `None` where they are set during the resolution.
fn handed_child<'c>(
    parent: &ResolvedAbility,
    child: &'c ResolvedAbility,
) -> Option<Cow<'c, ResolvedAbility>> {
    let produced_during_resolution = parent.forward_result
        || super::is_each_target_damage_sub(&child.effect)
        || (child.targets.is_empty()
            && parent.targets.is_empty()
            && super::effect_refs_parent_target(&child.effect));
    if produced_during_resolution {
        return None;
    }
    match super::one_sided_fight_subject_binding(parent, child) {
        // CR 120.1: the parent's object deals the damage, bound as the chain
        // resolver binds it.
        Some(subject @ super::OneSidedFightSubject::Prepend(_)) => {
            let mut handed = child.clone();
            super::bind_one_sided_fight_subject(&mut handed, subject);
            return Some(Cow::Owned(handed));
        }
        Some(super::OneSidedFightSubject::Illegal) => return None,
        None => {}
    }
    if !child.targets.is_empty() || parent.targets.is_empty() {
        return Some(Cow::Borrowed(child));
    }
    // CR 608.2c: the ordinary descent hands an undeclared child
    // `inherited_parent_targets`; a descent that waits on a player's choice
    // first hands it the parent's targets where `should_propagate_parent_targets`
    // holds. Answer only where the two agree.
    let inherited = super::inherited_parent_targets(parent, child);
    let after_a_choice = if super::should_propagate_parent_targets(parent, child) {
        parent.targets.clone()
    } else {
        Vec::new()
    };
    (inherited == after_a_choice).then(|| {
        let mut handed = child.clone();
        handed.targets = inherited;
        Cow::Owned(handed)
    })
}

/// A node that reads its trigger event or its parent's target when the copy
/// binds neither. It answers nothing, not what its resolver binds without a
/// referent: for a spell's unhanded `Counter { target: ParentTarget }`, the
/// spell itself.
fn names_unbound_referent(board: &GameState, bound: &ResolvedAbility) -> bool {
    bound.targets.is_empty()
        && board.current_trigger_event.is_none()
        && super::effect_parent_ref_slots(&bound.effect)
            .into_iter()
            .any(super::hydratable_event_context_filter)
}

/// CR 608.2c: a referent an earlier instruction of this entry's own resolution
/// produces (a tracked set, the token created, the card revealed or moved "this
/// way"). Before the entry resolves, that state belongs to other resolutions; a
/// tracked set read now is an earlier resolution's.
fn names_resolution_product(effect: &Effect) -> bool {
    super::effect_parent_ref_slots(effect)
        .into_iter()
        .any(|filter| {
            super::filter_references_tracked_set(filter)
                || crate::game::filter::filter_contains_last_created(filter)
                || crate::game::filter::filter_contains_last_zone_changed(filter)
                || crate::game::filter::filter_contains(filter, &|inner| {
                    matches!(inner, TargetFilter::LastRevealed)
                })
        })
}

/// CR 614.1 + CR 616.1: a replacement effect on the board may apply to an event
/// `bound`'s resolver proposes for something in `acted_on`. A replacement can
/// move the event to another object or player, or add an instruction that acts
/// on one; this authority does not apply replacements.
fn replacement_may_apply(
    board: &GameState,
    bound: &ResolvedAbility,
    acted_on: &[TargetRef],
) -> bool {
    proposed_events(board, bound, acted_on)
        .iter()
        .any(|event| !find_applicable_replacements(board, event, replacement_registry()).is_empty())
}

/// The events `bound`'s resolver proposes to the replacement pipeline for what
/// is in `acted_on`. Each amount is 1, which a proposed event of these kinds
/// always reaches.
fn proposed_events(
    board: &GameState,
    bound: &ResolvedAbility,
    acted_on: &[TargetRef],
) -> Vec<ProposedEvent> {
    let objects: Vec<ObjectId> = acted_on
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    let players = acted_on.iter().filter_map(|target| match target {
        TargetRef::Player(id) => Some(*id),
        TargetRef::Object(_) => None,
    });
    let source = Some(bound.source_id);
    let damage = |source_id: ObjectId, target: TargetRef| ProposedEvent::Damage {
        source_id,
        target,
        amount: 1,
        is_combat: false,
        applied: Default::default(),
    };
    let moves = |destinations: &[Zone]| -> Vec<ProposedEvent> {
        objects
            .iter()
            .filter_map(|id| board.objects.get(id).map(|object| (*id, object.zone)))
            .flat_map(|(id, from)| {
                destinations
                    .iter()
                    .map(move |to| ProposedEvent::zone_change(id, from, *to, source))
            })
            .collect()
    };
    match &bound.effect {
        // Damage from each object the resolver deals it from to each
        // recipient, the object itself among them ("and X damage to itself").
        Effect::DealDamage { .. } => super::deal_damage::damage_sources(board, bound)
            .into_iter()
            .flat_map(|source_id| {
                acted_on
                    .iter()
                    .map(move |target| damage(source_id, target.clone()))
            })
            .collect(),
        // CR 701.14a + CR 701.14c: each fighter deals damage to the other, and
        // a creature that fights itself deals damage to itself.
        Effect::Fight { .. } => super::fight::resolve_fight_fighters(board, bound)
            .ok()
            .flatten()
            .map(|(subject, fought)| {
                vec![
                    damage(subject, TargetRef::Object(fought)),
                    damage(fought, TargetRef::Object(subject)),
                ]
            })
            .unwrap_or_default(),
        Effect::Destroy {
            cant_regenerate, ..
        } => objects
            .iter()
            .map(|id| ProposedEvent::Destroy {
                object_id: *id,
                source,
                cant_regenerate: *cant_regenerate,
                applied: Default::default(),
            })
            .chain(moves(&[Zone::Graveyard]))
            .collect(),
        Effect::Sacrifice { .. } => objects
            .iter()
            .map(|id| ProposedEvent::Sacrifice {
                object_id: *id,
                player_id: bound.controller,
                applied: Default::default(),
            })
            .chain(moves(&[Zone::Graveyard]))
            .collect(),
        Effect::ChangeZone { destination, .. } => moves(&[*destination]),
        Effect::Bounce { destination, .. } => moves(&[destination.unwrap_or(Zone::Hand)]),
        Effect::Counter { .. } => moves(&[Zone::Graveyard, Zone::Exile, Zone::Hand, Zone::Library]),
        Effect::DiscardCard { .. } => objects
            .iter()
            .filter_map(|id| board.objects.get(id))
            .map(|object| ProposedEvent::Discard {
                player_id: object.owner,
                object_id: object.id,
                source_id: source,
                caused_by_effect: true,
                discard_frame: None,
                applied: Default::default(),
            })
            .chain(moves(&[Zone::Graveyard]))
            .collect(),
        Effect::LoseLife { .. } => players
            .map(|player_id| ProposedEvent::LifeLoss {
                player_id,
                amount: 1,
                applied: Default::default(),
            })
            .collect(),
        Effect::Mill { destination, .. } => players
            .map(|player_id| ProposedEvent::Mill {
                player_id,
                count: 1,
                destination: *destination,
                applied: Default::default(),
            })
            .collect(),
        Effect::PutCounter { counter_type, .. } | Effect::MultiplyCounter { counter_type, .. } => {
            objects
                .iter()
                .map(|id| ProposedEvent::AddCounter {
                    placement: CounterPlacement::Object {
                        actor: bound.controller,
                        object_id: *id,
                        counter_type: counter_type.clone(),
                    },
                    count: 1,
                    applied: Default::default(),
                })
                .collect()
        }
        Effect::RemoveCounter { counter_type, .. } => objects
            .iter()
            .filter_map(|id| board.objects.get(id))
            .flat_map(|object| {
                let kinds: Vec<_> = match counter_type {
                    Some(kind) => vec![kind.clone()],
                    None => object.counters.keys().cloned().collect(),
                };
                kinds
                    .into_iter()
                    .map(|counter_type| ProposedEvent::RemoveCounter {
                        object_id: object.id,
                        counter_type,
                        count: 1,
                        applied: Default::default(),
                    })
            })
            .collect(),
        Effect::SetTapState { state, .. } => objects
            .iter()
            .map(|id| match state {
                TapStateChange::Tap => ProposedEvent::Tap {
                    object_id: *id,
                    applied: Default::default(),
                },
                TapStateChange::Untap => ProposedEvent::Untap {
                    object_id: *id,
                    applied: Default::default(),
                },
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn objects(ids: Vec<ObjectId>) -> Vec<TargetRef> {
    ids.into_iter().map(TargetRef::Object).collect()
}

/// What `bound`'s resolver acts on. An arm calls the functions that resolver
/// binds with unless its comment says otherwise. `declared` is the same node
/// before any targets are handed to it.
///
/// NOTE: the `_` arm answers nothing. It covers every effect without an arm
/// here, the mass-population family among them. An effect a stack reader
/// inspects must be given an arm that calls its resolver's binding.
fn node_acted_on(
    state: &GameState,
    declared: &ResolvedAbility,
    bound: &ResolvedAbility,
) -> Vec<TargetRef> {
    match &bound.effect {
        Effect::Fight { .. } => super::fight::resolve_fight_fighters(state, bound)
            .ok()
            .flatten()
            .map(|(subject, fought)| vec![TargetRef::Object(subject), TargetRef::Object(fought)])
            .unwrap_or_default(),
        Effect::DealDamage { .. } => super::deal_damage::damage_recipients(state, bound),
        // CR 701.6a: the resolver removes the most recent entry whose id or
        // source is the target, which can be an entry other than the target.
        Effect::Counter { .. } => super::counter::countered_targets(state, bound)
            .into_iter()
            .filter(|target| match target {
                TargetRef::Object(id) => super::counter::countered_stack_index(state, *id)
                    .is_none_or(|index| state.stack[index].id == *id),
                TargetRef::Player(_) => true,
            })
            .collect(),
        Effect::Destroy { .. } => super::destroy::destroyed_targets(state, bound),
        Effect::Bounce { target, .. } => targeting::resolved_targets(bound, target, state)
            .into_iter()
            .filter(|target| matches!(target, TargetRef::Object(_)))
            .collect(),
        Effect::Pump { target, .. } => {
            let filter = super::resolved_object_filter(state, bound, target);
            objects(super::resolved_effect_object_ids(state, bound, &filter))
        }
        Effect::DoublePT { target, .. } => {
            objects(super::resolved_effect_object_ids(state, bound, target))
        }
        // The resolver exiles a `ParentTarget` found in a hand through the
        // chain's tracked set when that set holds a library card.
        Effect::ChangeZone {
            destination: Zone::Exile,
            target: TargetFilter::ParentTarget,
            ..
        } if bound.targets.iter().any(|target| {
            matches!(target, TargetRef::Object(id)
                if state.objects.get(id).is_some_and(|object| object.zone == Zone::Hand))
        }) =>
        {
            Vec::new()
        }
        // CR 610.3b: an object whose "until" event already happened does not
        // move.
        Effect::ChangeZone { .. }
            if bound
                .duration
                .as_ref()
                .and_then(Duration::zone_change_event)
                .is_some_and(|event| bound.context.duration_events.contains(&event)) =>
        {
            Vec::new()
        }
        Effect::ChangeZone { target, .. } => {
            let filter = targeting::resolve_tracked_set_sentinel(state, target.clone());
            let ids = super::resolved_effect_object_ids(state, bound, &filter);
            objects(crate::game::merge::expand_returned_merge_components(
                state, ids, &filter,
            ))
        }
        Effect::SetTapState {
            scope: EffectScope::Single,
            target,
            ..
        } => objects(super::tap_untap::tap_untap_target_ids(state, bound, target)),
        Effect::PutCounter { .. } | Effect::MultiplyCounter { .. } => {
            objects(super::counters::resolve_defined_or_targets(state, bound))
        }
        Effect::RemoveCounter { .. } => {
            objects(super::counters::counter_removal_targets(state, bound))
        }
        Effect::Goad { .. } => objects(super::goad::goad_targets(state, bound)),
        Effect::ForceBlock { target, .. } => objects(targeting::resolved_object_ids_for_filter(
            state, bound, target,
        )),
        Effect::PhaseOut { target } => {
            crate::game::ability_utils::collect_player_targets(state, bound, target)
                .into_iter()
                .map(TargetRef::Player)
                .chain(objects(super::phase_out::collect_object_targets(
                    state, bound, target,
                )))
                .collect()
        }
        Effect::LoseLife { target, .. } => vec![TargetRef::Player(
            super::life::resolve_life_loss_target(state, bound, target.as_ref()),
        )],
        Effect::Mill { target, .. } => vec![TargetRef::Player(
            super::resolve_player_for_context_ref(state, bound, target),
        )],
        // These resolvers bind inline rather than through a function shared
        // here: a node that declared targets is answered with them, and one
        // that would inherit its parent's is answered with nothing.
        Effect::Sacrifice { .. } | Effect::GenericEffect { .. } | Effect::DiscardCard { .. } => {
            declared.live_object_targets(state)
        }
        _ => Vec::new(),
    }
}

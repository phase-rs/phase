//! Effect-chain assembly: `EffectChainIr` → `AbilityDefinition`.
//!
//! Plan 01 §6 (Unit 6): the single source-order assembly traversal lives here.
//! This module was created by relocating `lower_effect_chain_ir`'s body from
//! `lower.rs` VERBATIM (U6-A) — a byte-identical move with zero behavior change,
//! so the arena / `AssemblyEnv` / antecedent-resolution layer can be built inside
//! this file in later increments without also moving code at the same time.
//!
//! The clause-lowering helpers this traversal calls still live in `lower.rs`
//! (widened to `pub(super)` for this move); relocating them is a later increment.

use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::effect_chain::{
    AbsorbKind, ClauseDisposition, ClauseId, EffectChainIr, OtherwiseKind, PlayerScopeRewrite,
    PriorModifier, ReplaceMeaningKind, ReplicateKind,
};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, CastFromZoneDriver,
    CastingPermission, ControllerRef, Effect, PlayerFilter, QuantityExpr, StaticCondition,
    SubAbilityLink, TapStateChange, TargetFilter,
};
use crate::types::game_state::TargetSelectionConstraint;
use crate::types::zones::Zone;

use super::conditions::ability_condition_to_static_condition;
use super::lower::{
    append_remember_card_to_standalone_exiled_choice, apply_where_x_ability_expression,
    apply_where_x_to_latest_def, attach_any_color_mana_rider_to_previous_play_from_exile,
    attach_cast_cost_raise_to_previous_play_from_exile,
    attach_graveyard_redirect_rider_to_prior_cast_from_zone,
    attach_land_enters_tapped_to_previous_play_from_exile, cast_cost_raise_rider,
    consolidate_die_and_coin_defs, definition_targets_self_source,
    effect_publishes_revealed_subject, extract_bounded_target_multi_target,
    extract_exact_target_multi_target, extract_optional_target_multi_target,
    extract_verb_up_to_multi_target, fold_copy_spell_gains_haste_and_quoted_grant,
    fold_deal_damage_then_prevent_into_computed_amount, fold_enters_this_way_counter_rider,
    fold_exile_resolving_rider, fold_search_choose_type_conditional_destination,
    fold_token_it_has_grants_into_token_statics, gate_other_revealed_card_on_multiplayer_reveal,
    gate_reflexive_rider_on_declined_optional_target, is_exile_until_cast_bottom_cleanup,
    is_land_enters_tapped_rider, is_linked_exile_cast_bottom_cleanup,
    is_spend_mana_as_any_color_rider, is_stable_branch_amount,
    nest_whenever_this_turn_token_cleanup_delayed_trigger,
    normalize_exile_until_cast_bottom_cleanup, normalize_linked_exile_cast_bottom_cleanup,
    parse_controlled_by_different_players_target_constraint,
    parse_same_zone_owner_target_constraint, parse_total_mana_value_target_constraint,
    patch_choose_from_zone_counter_continuation_target, patch_population_head_tap_anaphor,
    patch_self_ref_head_tap_anaphor, resolve_populated_token_anaphors,
    resolve_populated_unsuspect_anaphors, resolve_those_tokens_anaphors,
    rewire_result_anchored_subchain, rewrite_counter_instead_target_from_antecedent,
    rewrite_else_event_context_to_stable, rewrite_else_parent_target_to_self_ref,
    rewrite_player_anaphor_targets_in_definition, rewrite_those_tokens_from_antecedent,
    rewrite_two_target_counter_chain, target_choice_timing_for_clause,
    thread_chosen_damage_source_into_oneshot_effects,
};
use super::sequence::{apply_clause_continuation, def_bears_retargetable_copy};
use super::{
    append_to_deepest_sub_ability, apply_player_scope_rewrites,
    attach_alt_cost_to_prior_cast_from_zone, attach_mana_retention_to_prior_mana,
    attach_repeat_process_keywords, attach_same_is_true_keywords,
    bind_anaphoric_damage_subject_keep_recipient, collapse_ephemeral_color_choice_mana,
    contains_explicit_tracked_set_pronoun, contains_implicit_tracked_set_pronoun,
    def_is_damage_dealer, def_is_dig_look, def_is_dig_or_mill, def_is_generic_effect_head,
    def_is_keyword_counter_placement, demote_unbindable_batch_aggregate, draw_object_count_filter,
    fold_cast_copy_of_card_defs, has_explicit_player_target, inject_chosen_color_choice_grant,
    mark_uses_tracked_set, parse_spell_graveyard_replacement_rider,
    publishes_aggregate_set_from_resolution, publishes_tracked_set_from_resolution,
    rebind_tracked_aggregate_to_chain_set, retarget_counter_additional_cost_to_target,
    rewrite_grant_parent_to_filter, rewrite_parent_targets_to_tracked_set, rewrite_rounding_mode,
    rewrite_that_type_mana_instead, stamp_delayed_returns, try_fold_token_repeat_into_count,
    wire_optional_cast_decline_fallback,
};

// ===========================================================================
// AssemblyEnv (Plan 01 §6, U6-B1) — emit-time provenance + role registries
// ===========================================================================
//
// WRITE-ONLY THIS INCREMENT. Nothing reads these registries yet; the handlers
// still bind their antecedents positionally (`defs.last_mut()`, backward scans).
// U6-C flips the consumers over. Keeping population and consumption in separate
// commits means a bisect can never land on the bookkeeping.
//
// The shape here is dictated by the U6 `defs` audit:
//
//  * `origin` is `Option<ClauseId>`, NOT `ClauseId`. `apply_clause_continuation`
//    pushes nodes of its own (`sequence.rs` — e.g. the `SearchDestination`
//    `ChangeZone`), and those nodes belong to no clause. `FoldSearchIntoElse`
//    binds to exactly such a node, which is why a `ClauseId`-only arena key is
//    insufficient (audit §2). `origin: None` is that case, made concrete.
//
//  * Registries are recomputed against the CURRENT `defs` after every mutation
//    region, because `defs` is a `Vec` that handlers `pop()` and `mem::take()`.
//    Any index-keyed reference into it is invalidated by the next handler. That
//    fragility is not incidental — it is the argument for U6-C's stable arena
//    ids, and it is why these are recomputed rather than cached by index.
//
//  * These registries observe only TOP-LEVEL `defs` entries. Three of the real
//    consumers (`attach_alt_cost_to_prior_cast_from_zone`,
//    `attach_mana_retention_to_prior_mana`,
//    `find_prev_play_from_exile_permission_mut`) recurse INTO `sub_ability`
//    trees for the first node whose slot is still vacant (audit §6). They cannot
//    be served by a top-level registry at all — that is the node-granularity
//    decision U6-C must make first.

/// How a node in `defs` came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeRole {
    /// Emitted by a clause body on the normal (`Emit`) path.
    Primary,
    /// Built by a disposition handler out of a clause.
    HandlerProduct,
    /// Pushed by `apply_clause_continuation` — belongs to NO clause (audit §2).
    ContinuationProduct,
    /// Provenance lost because a continuation restructured `defs` in place
    /// (e.g. the Birthing Ritual `DigFromAmong` rebuild). Recorded honestly
    /// rather than guessed.
    Unknown,
}

// Written every clause, read in U6-C when the handlers bind by antecedent.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct NodeProvenance {
    /// `None` for continuation-pushed nodes — the audit's key finding.
    origin: Option<ClauseId>,
    role: NodeRole,
}

/// A registered antecedent: where it currently sits in `defs`, and where it came from.
// Written every clause, read in U6-C when the handlers bind by antecedent.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct NodeRef {
    index: usize,
    provenance: NodeProvenance,
}

// ---------------------------------------------------------------------------
// Arena (U6-C1) — DUAL-RUN, READ BY NOTHING
// ---------------------------------------------------------------------------
//
// Built and maintained alongside `defs`, which remains the sole source of truth
// for output. C1 is therefore a provable no-op: the arena cannot affect a single
// byte of the assembled `AbilityDefinition`. C2+ move the handlers over to it.
//
// What C1 exists to establish:
//  * `NodeId` is a STABLE, never-reused identity that survives `pop()` and
//    `mem::take()` — the ops that invalidate any index-keyed reference (the
//    reason U6-B1's registries had to be recomputed after every mutation).
//  * A node that leaves top-level `defs` is not deleted. It is `Absorbed { into }`
//    — nested under a known parent. Design §2.2: a node
//    can migrate OUT of the node-set (nested by a handler, or wrapped into an
//    `Effect` payload), and a registry that later binds to such a node would
//    mutate a def that is no longer in the output. That must be a typed state,
//    not an invariant we hope holds.

/// Stable identity for an assembled node. NOT a `defs` index — indices are
/// invalidated by the very next `pop()`/`mem::take()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct NodeId(u32);

/// Where a node stands relative to top-level `defs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeStatus {
    /// Present in `defs` right now.
    Live,
    /// No longer top-level: nested under `into` (as its `sub_ability`/`else_ability`)
    /// by a handler that popped it and pushed its replacement.
    Absorbed { into: NodeId },
}

#[allow(dead_code)]
#[derive(Debug)]
struct ArenaNode {
    prov: NodeProvenance,
    status: NodeStatus,
    /// Identity witness, stamped ONCE at birth — see [`def_witness`].
    witness: DefWitness,
}

/// A def's identity, as something `assert_mirrors` can actually compare against
/// `defs` — the heap address of its boxed `Effect`.
///
/// This is the only witness available that is invariant under exactly the
/// operations assembly performs on a def that SURVIVES, and variant under the one
/// that creates a new def:
///
///  * `Vec::remove` / `pop` / `push` / `mem::take` move the `AbilityDefinition`
///    struct, but never the pointee of its `Box<Effect>`.
///  * The in-place effect rewrites (`*previous.effect = Effect::ExileTop { .. }`,
///    `sequence.rs`) overwrite the pointee's CONTENTS, not its address — so a
///    witness survives them, exactly as `NodeId` claims identity does.
///  * A genuinely new def (`AbilityDefinition::new`) allocates a new `Box`, so it
///    gets a new witness — which is the correct outcome: it also gets a new id.
///
/// It is stamped when the node is created and NEVER re-stamped. Re-deriving it
/// from `defs` on every `observe` would make the mirror assert compare `defs`
/// against itself — a tautology, which is the defect this exists to remove.
///
/// The one way an address could lie is ABA: a def is dropped, its `Box` freed, and
/// a later def allocated at the same address. It cannot produce a false green here,
/// because the asserts only ever compare witnesses of defs that are still ALIVE —
/// a node in `order` (its def is in `defs`) or an `Absorbed` node (its def was
/// MOVED into a parent's `sub_ability`/`else_ability`, so its `Box` is not freed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DefWitness(usize);

fn def_witness(def: &AbilityDefinition) -> DefWitness {
    DefWitness(std::ptr::from_ref(&*def.effect) as usize)
}

/// Stable-id mirror of `defs`. `order` is the live top-level sequence; it is the
/// ONLY positional truth in the arena, and it is append-only in the sense that
/// matters (a node leaving `order` keeps its id and gains a terminal status).
#[allow(dead_code)]
#[derive(Debug, Default)]
struct Arena {
    nodes: Vec<ArenaNode>,
    order: Vec<NodeId>,
    /// Nodes popped out of `order` this clause, awaiting classification by `settle`.
    detached: Vec<NodeId>,
}

impl Arena {
    fn node(&self, id: NodeId) -> &ArenaNode {
        &self.nodes[id.0 as usize]
    }

    fn push_new(
        &mut self,
        origin: Option<ClauseId>,
        role: NodeRole,
        witness: DefWitness,
    ) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(ArenaNode {
            prov: NodeProvenance { origin, role },
            status: NodeStatus::Live,
            witness,
        });
        self.order.push(id);
        id
    }

    /// Return a MOVED def to top-level under its ORIGINAL `NodeId`.
    ///
    /// Identity tracks the def, not its position (U6-C2 ruling). A def that
    /// survives into the output keeps its id even when re-parented or moved by
    /// `mem::take`; only a genuinely NEW def gets a fresh id. Otherwise a binding
    /// taken against that node before the move would silently resolve to nothing —
    /// or to a *different* node — which is exactly the silent-no-op class
    /// `Absorbed { into }` exists to prevent (it already means "same node, now
    /// nested"; identity churn on re-parenting would contradict that model).
    ///
    /// Used by the two handlers that pop a def and push *that same def* back:
    /// `Instead` (root = `chain_defs.remove(0)`, moved) and
    /// `ModifyPrior::EntersTappedAttacking` (`patched` IS the popped def, mutated).
    fn reinstate(&mut self, id: NodeId) {
        self.detached.retain(|d| *d != id);
        self.nodes[id.0 as usize].status = NodeStatus::Live;
        self.order.push(id);
    }

    /// The id currently at top-level position `index`, if any.
    pub(super) fn id_at(&self, index: usize) -> Option<NodeId> {
        self.order.get(index).copied()
    }

    /// Every top-level id EXCEPT the first. `Instead` nests defs `1..N` into the root
    /// (`defs[0]`) and must name it as their parent, so it needs exactly this set.
    fn tail_ids(&self) -> Vec<NodeId> {
        self.order.iter().skip(1).copied().collect()
    }

    /// Provenance of the node currently at top-level `index` — the SINGLE store.
    ///
    /// There used to be a SECOND, positional one (`AssemblyEnv::prov`), kept in step
    /// by `observe` with `truncate` + re-push. `reinstate` preserved identity in the
    /// arena's store — but `refresh`, which builds ALL role membership, read the
    /// POSITIONAL one. So the identity machinery was faithfully maintaining a store
    /// that role membership never consulted, and the two silently disagreed after
    /// any move: `Instead` re-stamped `prov[0]` with the OVERRIDE clause's id while
    /// the arena still (correctly) held clause 0's. Keying provenance to identity in
    /// one place removes the disagreement CLASS, not just its instances.
    fn prov_at(&self, index: usize) -> Option<NodeProvenance> {
        self.order.get(index).map(|id| self.node(*id).prov)
    }

    /// Mirror a MID-VECTOR removal (`defs.remove(index)`).
    ///
    /// `sync_len` can only shrink from the TAIL — it `pop()`s `order`, and `observe`
    /// documents that precondition in its own doc comment. Mirroring a mid-vector
    /// `defs.remove(i)` with a tail pop keeps `order.len()` exactly right while
    /// detaching the WRONG node and silently renaming every node from `i` onward.
    /// A removal is a different operation from a truncation, so it gets its own
    /// primitive rather than being approximated by one.
    fn remove_at(&mut self, index: usize) -> NodeId {
        let id = self.order.remove(index);
        self.detached.push(id);
        id
    }

    /// Record that `id` was nested INTO `parent` by the handler that moved it.
    ///
    /// `settle` otherwise INFERS the parent as `order.last()`. That inference holds
    /// only when the handler pushes its replacement and nothing else. A handler that
    /// ALSO runs an intrinsic continuation pushes a node after the real parent, and
    /// the inference then names the continuation's node. A site that knows its parent
    /// states it; emission order is not a parenthood oracle.
    fn absorb(&mut self, id: NodeId, parent: NodeId) {
        self.detached.retain(|d| *d != id);
        self.nodes[id.0 as usize].status = NodeStatus::Absorbed { into: parent };
    }

    /// Mirror a `defs` length change we cannot hook from inside (helper-driven
    /// pushes in `apply_clause_continuation` / `attach_repeat_process_keywords`,
    /// and every handler pop). Ids are issued, never recycled.
    fn sync_len(&mut self, defs: &[AbilityDefinition], origin: Option<ClauseId>, role: NodeRole) {
        while self.order.len() > defs.len() {
            let id = self
                .order
                .pop()
                .expect("order non-empty while longer than defs");
            self.detached.push(id);
        }
        while self.order.len() < defs.len() {
            // The node being created IS `defs[order.len()]` — stamp its witness at
            // birth, from the def it actually mirrors. Never re-stamp an existing
            // node: a witness re-derived from `defs` would make `assert_mirrors`
            // compare `defs` against itself.
            let witness = def_witness(&defs[self.order.len()]);
            self.push_new(origin, role, witness);
        }
    }

    /// End of a clause: every def this clause detached must have been RESOLVED.
    ///
    /// A handler that removes a def from `defs` has to say where it went — `absorb`
    /// when it nests the def under another, `reinstate` when it returns the def to
    /// top-level. All four Phase-1 shrink sites do: `EntersTappedAttacking` and
    /// `Instead` reinstate; `DigAlt`, `Instead` and `FoldSearchIntoElse` absorb.
    ///
    /// U6-0c marked whatever was still detached here as `Dropped`. That was a
    /// REINTRODUCED SILENT LIE, and it is the reason this asserts instead. The
    /// parenthood assert SKIPS `Dropped` nodes — nothing to check, the def left the
    /// output — so a future handler that detaches a def, nests it, and forgets to
    /// `absorb` would be recorded as "removed from the output" and quietly accepted.
    /// The arena would hold the wrong model and no assert would object: exactly the
    /// class of defect U6-0 exists to delete, reintroduced by the fix for it.
    ///
    /// So `Dropped` is gone. A node is `Live` or `Absorbed`, and "the handler never
    /// said" is a failure, not a third state.
    fn settle(&mut self) {
        debug_assert!(
            self.detached.is_empty(),
            "arena/defs divergence: a detached node was never absorbed or reinstated — \
             the handler that removed it from `defs` did not say where it went"
        );
        self.detached.clear();
    }

    /// Follow an `Absorbed` chain up to the LIVE top-level node that still holds this
    /// node in its tree, and return that node's index in `defs`.
    fn live_root_index(&self, id: NodeId) -> Option<usize> {
        let mut cursor = id;
        // Bounded by the node count: a parent is always an EXISTING node, so a chain
        // that has not terminated within `nodes.len()` hops must contain a cycle.
        for _ in 0..=self.nodes.len() {
            match self.node(cursor).status {
                NodeStatus::Live => return self.order.iter().position(|o| *o == cursor),
                NodeStatus::Absorbed { into } => cursor = into,
            }
        }
        // A cycle looks unconstructible (`absorb` is only ever called with a parent
        // that is Live at the time), but it must not FAIL QUIET: returning `None` here
        // would make the absorbed-parenthood assert pass VACUOUSLY for this node —
        // a self-inflicted false green of exactly the kind U6-0 exists to remove.
        unreachable!("arena: `Absorbed` chain contains a cycle — parenthood is corrupt")
    }

    /// The arena's LIVE nodes correspond 1:1 to `defs` — *by identity*, not merely
    /// by count.
    ///
    /// `debug_assert` so it is free in release but active in the entire test suite
    /// and the full-pool corpus sweep. A divergence here is a finding, not a nit:
    /// it means the arena does not model what assembly actually does.
    ///
    /// **Why this takes `&[AbilityDefinition]` and not `defs_len`.** The four count
    /// and status asserts below are all maintained by the same two writes that
    /// establish them (`sync_len` reconciles `order.len()` TO `defs_len`, then the
    /// assert compares them; `settle` drains `detached`, then asserts it is empty).
    /// They cannot fail by construction — they prove the assert is *wired*, never
    /// that it can *discriminate a wrong model*. Two real modelling defects shipped
    /// underneath them. The two asserts that follow compare the arena against
    /// `defs` and against the output tree, so a wrong model has something to be
    /// wrong ABOUT.
    fn assert_mirrors(&self, defs: &[AbilityDefinition]) {
        debug_assert_eq!(
            self.order.len(),
            defs.len(),
            "arena/defs divergence: live node count != defs len"
        );
        debug_assert!(
            self.detached.is_empty(),
            "arena/defs divergence: detached nodes left unclassified"
        );
        debug_assert!(
            self.order
                .iter()
                .all(|id| matches!(self.node(*id).status, NodeStatus::Live)),
            "arena/defs divergence: a non-Live node is in `order`"
        );
        // Live-by-status and live-by-`order` are the same SET. Counting is O(n) rather
        // than the O(nodes x order) `contains`-in-a-loop it replaces — and this assert
        // is LIVE in the full-pool export (`[profile.tool] inherits = "dev"`).
        //
        // Precisely: standalone, the count is INCOMPARABLE to the membership scan, not
        // stronger. It catches a duplicate id in `order` (which inflates the length)
        // that a `contains` scan waves through; it is blind to a duplicate that is
        // exactly compensated by an orphaned Live node. That blind spot is covered —
        // the identity assert below pins `order[i]` to `defs[i]` by witness, and an
        // orphan cannot survive it. The SET of asserts is what holds, not this line.
        debug_assert_eq!(
            self.nodes
                .iter()
                .filter(|n| matches!(n.status, NodeStatus::Live))
                .count(),
            self.order.len(),
            "arena/defs divergence: Live status and `order` membership disagree"
        );

        // IDENTITY, not count: `order[i]` must still name the def that is ACTUALLY
        // at `defs[i]`. A `defs` mutation mirrored by the WRONG arena op — a
        // mid-vector `Vec::remove` mirrored by `sync_len`'s tail `pop` — keeps every
        // count above perfectly balanced while silently renaming every node from the
        // removal point on. This is the assert that notices.
        debug_assert!(
            self.order
                .iter()
                .zip(defs)
                .all(|(id, def)| self.node(*id).witness == def_witness(def)),
            "arena/defs divergence: `order` names a different def than `defs` holds \
             at that index — a `defs` mutation was mirrored by the wrong arena op"
        );

        // `Absorbed { into }` is a CLAIM about the output tree, and the claim is
        // checkable: the absorbed def must actually BE somewhere inside the tree of
        // the node it names. `settle` INFERS that parent from emission order
        // (`order.last()`), which is a guess — a handler that pushes anything AFTER
        // its real parent (an intrinsic continuation, say) makes the guess wrong.
        // This is the assert that catches an inferred parent being the wrong one.
        debug_assert!(
            self.nodes.iter().all(|n| match n.status {
                NodeStatus::Absorbed { into } => self
                    .live_root_index(into)
                    .is_none_or(|i| def_tree_contains(&defs[i], n.witness)),
                NodeStatus::Live => true,
            }),
            "arena/defs divergence: an `Absorbed {{ into }}` node is not anywhere \
             inside the tree of the parent it names — the parent was inferred, wrongly"
        );
    }
}

/// Is a def with this witness anywhere in `def`'s tree? Walks the two slots a
/// handler can nest an absorbed node into.
///
/// `AbilityDefinition` has a THIRD nested-def slot — `mode_abilities` — and it is
/// excluded deliberately, not by oversight: assembly never writes it (verified; the
/// parser only ever reads/iterates it), so no absorbed node can land there. If a
/// future handler does nest into it, this walk will fail to find the node and the
/// absorbed-parenthood assert goes RED — the safe direction, and the signal to add
/// the slot here rather than a silent pass.
fn def_tree_contains(def: &AbilityDefinition, witness: DefWitness) -> bool {
    def_witness(def) == witness
        || [def.sub_ability.as_deref(), def.else_ability.as_deref()]
            .into_iter()
            .flatten()
            .any(|child| def_tree_contains(child, witness))
}

/// Emit-time facts about the chain assembled so far.
//
// Fields are populated but not yet read (U6-C consumes them); `dead_code` is
// expected and intentional for exactly one increment.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(super) struct AssemblyEnv {
    /// U6-C1 arena. Maintained in lockstep with `defs`, and the SINGLE authority for
    /// node identity and provenance — see `Arena::prov_at`.
    arena: Arena,
    /// "Look at the top N"-class antecedent (`Dig`/`Mill`/`RevealUntil`) — the
    /// node `RestDestination` / `DigFromAmong` continuations scan backward for.
    last_dig: Option<NodeRef>,
    /// `Destroy`/`DestroyAll` — the `CantRegenerate` rider's antecedent.
    last_destroy_like: Option<NodeRef>,
    last_cast_from_zone: Option<NodeRef>,
    last_mana: Option<NodeRef>,
    last_play_from_exile: Option<NodeRef>,
    /// A node whose `condition` is ACTUALLY set — see `refresh` for why this is
    /// read off the built def and never off `ClauseIr::condition`.
    last_conditional: Option<NodeRef>,
    /// An "any player may" head (`optional_for`) — `BranchOtherwise`'s fallback antecedent.
    last_optional_for: Option<NodeRef>,
    /// The continuation-pushed search-destination `ChangeZone` that
    /// `FoldSearchIntoElse` folds into its `else_ability`.
    last_search_destination: Option<NodeRef>,

    // ---- U6-C2 binding registries -------------------------------------------
    // Lists, not single slots: a guarded selector may have to walk PAST a
    // candidate that fails its guard. `BranchOtherwise`'s fallback scan does
    // exactly that (`optional_for.is_some() && sub_ability.is_none()` — the
    // nearest optional head may already have a sub, in which case the live scan
    // keeps going). A "last only" registry cannot reproduce that, so the walk is
    // over a typed candidate list — never over the output tree.
    /// Every top-level node whose `condition` is actually set, in emission order.
    conditional_nodes: Vec<usize>,
    /// Every top-level "any player may" head, in emission order.
    optional_head_nodes: Vec<usize>,
    /// Every continuation-pushed search-destination `ChangeZone`, in emission order.
    search_destination_nodes: Vec<usize>,
    /// Every `Dig`/`RevealUntil` node (the `RestDestination` patchable set).
    dig_or_reveal_until_nodes: Vec<usize>,
    /// Every `Destroy`/`DestroyAll` node — including those nested inside a
    /// `CreateDelayedTrigger` wrapper (Merieke Ri Berit) — that forms the
    /// can't-be-regenerated antecedent set (see `effect_wraps_destroy_like`).
    destroy_like_nodes: Vec<usize>,
    /// Every node already carrying a face-down profile (CR 708.2a spec antecedent).
    face_down_profile_nodes: Vec<usize>,
}

/// Which antecedent a handler is binding to. Point selectors only (U6-C2);
/// the range selector lands in C3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AntecedentSelector {
    /// The most recently emitted top-level node ("the prior def").
    LastEmitted,
    /// The FIRST emitted node. `Instead` alone binds this — CR 608.2c: the
    /// override replaces the first printed instruction. Do not unify with `Last*`.
    FirstEmitted,
    /// The most recent node registered under a role, walking back past any
    /// candidate that fails the guard.
    LastWithRole(AntecedentRole),
    /// **EVERY node registered under a role** — the one FAN-OUT binding in the
    /// assembler. Bound with [`AssemblyEnv::resolve_all`]; [`AssemblyEnv::resolve`]
    /// rejects it, because a point binding cannot express it.
    ///
    /// Grammatical class, not a card: a PLURAL quantifier ("you may choose new
    /// targets for the copIES") scopes over the SELECTOR, not merely over the
    /// mutation the selected node undergoes. When the things it quantifies over land
    /// in SIBLING defs — Banish into Fable's two conditional copies, Ulalek's
    /// spells-then-abilities pair — a `LastWithRole` binding reaches only the last of
    /// them and every earlier one silently keeps its original targets (CR 707.10c).
    ///
    /// Membership is the SAME predicate/registry split as `LastWithRole` (see
    /// `live_role_predicate`) — this selector changes only how many candidates are
    /// taken, never which nodes qualify. That is deliberate: a fan-out whose
    /// membership drifted from the point selector's would be a second, undeclared
    /// role.
    ///
    /// Note the SWALLOW trap that haunts `LastWithRole` does NOT apply here, and the
    /// direction is worth stating: `LastWithRole` STOPS the walk at the node it binds,
    /// so an over-generous role hides a real antecedent further back. A fan-out never
    /// stops, so an extra member cannot hide an earlier one. The exposure is the
    /// MIRROR risk — patching a node that should have been left alone — which is why
    /// the plural clause must only be bound to defs emitted BEFORE it. That holds by
    /// construction: continuations are applied in SOURCE ORDER as clauses stream, so
    /// `defs` at binding time contains exactly the copies already stated (a later
    /// mode's copy — Choreographed Sparks' retarget-less mode 2 — does not exist yet).
    AllWithRole(AntecedentRole),
    /// A node pushed by a continuation rather than a clause body — it has NO
    /// `ClauseId` (audit §2), so it can only be named by role + provenance.
    ContinuationProduct(ContinuationRole),
    /// **A lookback-transparent intervening clause between an anchor and its
    /// dependent** — the one RANGE binding in the assembler.
    ///
    /// Grammatical class, not a card: a dependent clause binds back to an anchor
    /// (e.g. a "look at the top N" `Dig`), but a *transparent* clause may sit
    /// BETWEEN them in written order — one the dependent looks straight through.
    /// The dependent must therefore find the first qualifying clause emitted
    /// AFTER the anchor, not simply the previous def. CR 608.2c: instructions are
    /// followed in written order, so the intervening clause is real and ordered,
    /// not noise to be skipped.
    ///
    /// Resolved over `order` (append-only EMISSION provenance) — never over the
    /// output tree, so the no-recursive-inspection rule holds. Unlike every other
    /// selector this walks FORWARD from the anchor and takes the FIRST match.
    ///
    /// Full-pool hit count: **2 of 35,034 cards** (measured, not estimated).
    /// Birthing Ritual is an instance of the class, not its name. A two-card
    /// selector is accepted KNOWINGLY: the alternative is leaving a raw positional
    /// scan in the assembler, which reintroduces exactly the coupling U6 removes.
    Between { anchor: NodeId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AntecedentRole {
    /// `condition` actually set on the built def (never read off `ClauseIr`).
    Conditional,
    /// An "any player may" head (`optional_for`).
    OptionalHead,
    /// A `Dig` or `RevealUntil` — the set `patch_rest_destination_recursively`
    /// can patch, which a `RestDestination` ("put the rest ...") clause binds back
    /// to. Deliberately a DIFFERENT set from `DigOrMill`.
    DigOrRevealUntil,
    /// A `Destroy` or `DestroyAll` — the antecedent of a "can't be regenerated"
    /// rider, which may be non-adjacent (Kirtar's Wrath puts a Token between them)
    /// and may be nested inside a `CreateDelayedTrigger` wrapper (Merieke Ri
    /// Berit's leaves-battlefield/becomes-untapped delayed destroy).
    DestroyLike,
    /// A move/manifest/turn-face-down node that ALREADY carries a face-down
    /// profile — the antecedent a "They're N/M ... creatures." spec refines
    /// (CR 708.2a). "Already carries one" is the binding condition itself, not a
    /// guard: a node with no profile is not this antecedent at all.
    FaceDownProfileHolder,
    /// A `Dig` or `Mill` — the "look at / mill N cards" anchor a `DigFromAmong`
    /// continuation binds back to.
    ///
    /// NOTE the set is deliberately NARROWER than the `last_dig` registry, which
    /// is a `Dig|Mill|RevealUntil` union. The two consumers want DIFFERENT sets
    /// (`RestDestination` wants `Dig|RevealUntil`), so a shared union registry
    /// would mis-bind. Roles name a set, not a vibe.
    DigOrMill,

    // ---- U6-C5 LIVE roles ---------------------------------------------------
    // Membership is recomputed from `defs` on every `resolve` (see
    // `live_role_predicate`), never cached in a registry.
    /// A `GenericEffect` head — the template a "The same is true for <keywords>"
    /// continuation replicates its first `StaticDefinition` from (CR 702).
    ///
    /// Membership is the EFFECT VARIANT ALONE. It deliberately does NOT require a
    /// non-empty `static_abilities`: the pre-arena scan stopped (`return`) at the
    /// first `GenericEffect` from the back and gave up when it carried no statics —
    /// it did NOT keep walking to an earlier one. Narrowing the role to "has a
    /// static" would resume that walk and bind a DIFFERENT def. The empty case is
    /// the mutator's bail, not the role's filter.
    GenericEffectHead,
    /// A keyword-counter placement (`PutCounter { counter_type: Keyword(_) }`) —
    /// the sibling template a "Repeat this process for <keywords>" continuation
    /// clones (Kathril, Aspect Warper).
    KeywordCounterPlacement,
    /// A `DealDamage` — the antecedent an "excess damage" rider redirects from
    /// (CR 120.4a). The rider need not be adjacent to the damage clause, which is
    /// why this is a role and not `LastEmitted`.
    ///
    /// Membership is the EFFECT VARIANT ALONE. It deliberately does NOT require
    /// `excess.is_none()`: the scan this replaces stopped at the nearest
    /// `DealDamage` unconditionally and then overwrote `excess`. Narrowing the role
    /// to "has no excess yet" would make an already-written def a NON-candidate and
    /// resume the walk to an earlier `DealDamage` the old code never reached. The
    /// overwrite is the mutator's business; the role names where the walk STOPS.
    DamageDealer,
    /// A `Dig` — the "look at the top N" anchor that BOTH private-look riders bind
    /// back to: `ExileLookedAtCard` (the Gonti impulse idiom, CR 608.2c) rewrites it
    /// into an `ExileTop`, and `ExileOneOfThemFaceDown` (Hideaway, CR 702.75a)
    /// patches it into the choose-one-and-exile shape. One role, because the two
    /// scans it replaces had BYTE-IDENTICAL predicates — they are the same
    /// antecedent, named twice.
    ///
    /// Membership is the EFFECT VARIANT ALONE — NOT `reveal: false`. The private-look
    /// requirement (CR 701.20e) is a filter the RECOGNIZERS already applied upstream
    /// when they decided to emit these continuations at all; importing it here would
    /// make a revealed `Dig` a non-candidate and walk PAST it to an earlier one the
    /// old scans never reached. Same trap, mirrored, as narrowing `GenericEffectHead`
    /// to "has a static".
    DigLook,
    /// A def whose effect TREE bears a `CopySpell` — the antecedent a "you may choose
    /// new targets for the copy/copies" sentence binds back to (CR 707.10c). A clause
    /// that resolves between the copy and the sentence may sit in between (Narset's
    /// Reversal's bounce, Spinerock Tyrant's wither rider), which is why this is a
    /// role and not `LastEmitted`.
    ///
    /// The FIRST TREE-RECURSIVE role. Every role above is a property of a def's own
    /// top-level effect (`DamageDealer` already peeks one level, through a `TargetOnly`
    /// head); this one is a property of the def's whole effect SUBTREE, because the
    /// `CopySpell` it names can be nested arbitrarily deep — under a
    /// `CreateDelayedTrigger` wrapper (Galvanic Iteration) or down the `sub_ability`
    /// chain (the Chain cycle nests the optional copy under the parent discard).
    ///
    /// This does NOT breach the "no recursive inspection of the output" rule. That rule
    /// forbids a handler from walking the output GRAPH — sideways across `defs`, or
    /// through parent/sibling links — to discover WHICH node is its antecedent. Here the
    /// search across `defs` is exactly the declared `LastWithRole` walk, and membership
    /// of each candidate is a property of that candidate ALONE, computed from its own
    /// subtree. Node-local, just deeper than one level.
    ///
    /// Membership mirrors the mutator's descent EXACTLY — see
    /// `sequence::def_bears_retargetable_copy`. Widening it (e.g. descending
    /// `else_ability`, which the mutator does not) would bind a def the mutator cannot
    /// patch, and because the walk STOPS at the bound node the retarget would be
    /// silently dropped instead of reaching the real copy further back. Same trap as
    /// narrowing `GenericEffectHead`, in the opposite direction.
    CopySpellBearer,
}

/// Membership predicates for the roles whose candidacy is **live** — recomputed
/// from `defs` on every `resolve` instead of read from a `refresh`-maintained
/// registry. Returns `None` for the registry-backed roles (U6-C2/C3).
///
/// Why the split is a CORRECTNESS requirement, not a style choice: `refresh` only
/// runs from `observe`, and `observe` is only called where **`defs.len()` changes**.
/// These roles are sensitive to mutations that change no length at all — a
/// `KeywordOverride` nesting a `sub_ability` in place (and, from U6-C5c, an
/// `alt_ability_cost`/`expiry` slot being filled). A cached registry for them would
/// be stale by construction; a live predicate cannot be.
///
/// Each predicate is a STRUCTURAL MIRROR of its mutator's success condition and
/// lives beside it, so the two cannot drift apart unnoticed.
fn live_role_predicate(role: AntecedentRole) -> Option<fn(&AbilityDefinition) -> bool> {
    match role {
        AntecedentRole::GenericEffectHead => Some(def_is_generic_effect_head),
        AntecedentRole::KeywordCounterPlacement => Some(def_is_keyword_counter_placement),
        // LIVE, not cached. The scan this role replaces (`sequence.rs`, the
        // `DigFromAmong` fallthrough) re-derived its antecedent from `defs` on every
        // call, so it saw the CURRENT effect of every def. A cached registry is
        // refreshed only by `observe` — i.e. only when `defs.len()` changes — and the
        // assembler performs LENGTH-PRESERVING in-place effect rewrites
        // (`*previous.effect = Effect::ExileTop { .. }`) that turn a `Dig` into
        // something else without any length change. Cached, this role would go on
        // naming a def that is no longer a `Dig`/`Mill`. Live, it is EXACTLY the scan.
        AntecedentRole::DigOrMill => Some(def_is_dig_or_mill),
        // LIVE, for the same reason as `DigOrMill` — and it is the SAME rewrite that
        // forces it: `ExileLookedAtCard` does `*previous.effect = Effect::ExileTop`,
        // turning a `Dig` into a non-`Dig` with NO length change, so `observe` never
        // fires and a cached registry would go on naming it. `ExileOneOfThemFaceDown`
        // nests a `sub_ability` in place — also length-preserving. Live, the role is
        // EXACTLY the scan it replaces.
        AntecedentRole::DigLook => Some(def_is_dig_look),
        // LIVE. `ExcessDamageToController` writes a FIELD (`*excess = Some(..)`) — the
        // most length-preserving mutation there is. A cached registry is refreshed only
        // where `defs.len()` changes, so it could not see this at all; that it happens
        // to stay correct for THIS role today is luck, not design. Cached membership is
        // stale by construction here, so it is not offered.
        AntecedentRole::DamageDealer => Some(def_is_damage_dealer),
        // LIVE — and this role could not be cached even in principle. Its membership is
        // a property of a def's whole effect SUBTREE, and the assembler moves effects
        // INTO subtrees without touching `defs.len()`: nesting a `sub_ability` in place
        // can make a def that was NOT a copy-bearer into one (the Chain cycle hangs the
        // optional `CopySpell` under the parent discard). `observe` only refreshes where
        // the length changes, so a registry would miss that node's birth entirely — not
        // merely go stale on a rewrite, but never see it. Live, the role IS the scan.
        AntecedentRole::CopySpellBearer => Some(def_bears_retargetable_copy),
        AntecedentRole::Conditional
        | AntecedentRole::OptionalHead
        | AntecedentRole::DigOrRevealUntil
        | AntecedentRole::DestroyLike
        | AntecedentRole::FaceDownProfileHolder => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContinuationRole {
    /// The search-destination `ChangeZone` a `SearchDestination` continuation pushes.
    SearchDestination,
}

/// Evaluated against the BOUND NODE ONLY — never by walking the output graph, so
/// the "handlers may not recursively inspect the output" rule holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindGuard {
    /// The node must not already carry a `sub_ability` (mutable output state —
    /// an earlier handler may have filled it).
    NoSubAbility,
    /// The node is an optional, lookback-transparent cost clause
    /// (`Sacrifice`/`PayCost`) — the "intervening clause" half of `Between`.
    DigLookbackTransparentCost,
    /// The node's effect must be of this class. Encodes the shape-gated SILENT
    /// no-op in the type: if the prior def is the wrong shape the binding simply
    /// misses, and `OnMiss::Ignore` makes the handler do nothing — exactly what
    /// the pre-arena code did with an inline `matches!` + no else branch.
    EffectShape(EffectClass),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EffectClass {
    /// `CopyTokenOf` | `Token` | `ChangeZone` | `Meld` — the effects
    /// `ModifyPrior::EntersTappedAttacking` can patch.
    PermanentCreator,
    /// `ChooseDrawnThisTurnPayOrTopdeck` — the only effect
    /// `DrawnThisTurnFollowup` patches.
    DrawnThisTurnChoice,
}

/// What a handler does when its antecedent does not resolve.
///
/// **Deliberately has no `Default`.** A new binding site cannot compile without
/// consciously choosing. `ModifyPrior::EntersTappedAttacking` and
/// `DrawnThisTurnFollowup` are shape-gated SILENT no-ops today — a binding layer
/// that failed loudly on a miss would itself be a behavior change, and it would
/// fire on real cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnMiss {
    /// Do nothing. The clause contributes no def and no error.
    Ignore,
}

impl AssemblyEnv {
    /// The stable id of the node currently at top-level `index`.
    pub(super) fn node_id_at(&self, index: usize) -> Option<NodeId> {
        self.arena.id_at(index)
    }

    /// Mirror any length change `defs` has undergone since the last call into the
    /// arena, then recompute the registries against the current `defs`.
    ///
    /// Handles growth (push/extend) and TAIL shrink (`pop`, `mem::take`). A
    /// MID-VECTOR `defs.remove(i)` is NOT a length change this can mirror — the
    /// caller must announce it with `Arena::remove_at` first, which is why that is a
    /// separate primitive rather than something inferred from a length delta.
    pub(super) fn observe(
        &mut self,
        defs: &[AbilityDefinition],
        origin: Option<ClauseId>,
        role: NodeRole,
    ) {
        self.arena.sync_len(defs, origin, role);
        self.refresh(defs);
    }

    /// Recompute the role registries. Last match wins — these are "the most
    /// recent X", mirroring the backward scans they will eventually replace.
    fn refresh(&mut self, defs: &[AbilityDefinition]) {
        self.last_dig = None;
        self.last_destroy_like = None;
        self.last_cast_from_zone = None;
        self.last_mana = None;
        self.last_play_from_exile = None;
        self.last_conditional = None;
        self.last_optional_for = None;
        self.last_search_destination = None;
        self.conditional_nodes.clear();
        self.optional_head_nodes.clear();
        self.search_destination_nodes.clear();
        self.dig_or_reveal_until_nodes.clear();
        self.destroy_like_nodes.clear();
        self.face_down_profile_nodes.clear();

        for (index, def) in defs.iter().enumerate() {
            // Provenance comes from the arena, keyed by IDENTITY — so a def that was
            // moved (`Instead`'s root, `EntersTappedAttacking`'s patched def) keeps
            // the provenance it was born with, and role membership below is computed
            // from the truth `reinstate` has been preserving all along.
            let provenance = self.arena.prov_at(index).expect(
                "`observe` syncs `order` to `defs` before `refresh`, so every index is live",
            );
            let node = NodeRef { index, provenance };

            // CRITICAL (audit §5.1): register a conditional off the BUILT def's
            // `condition`, never off `ClauseIr::condition`. The `Emit` path
            // deliberately leaves `def.condition` unset for a `GenericEffect`
            // whose condition was pushed down onto its `StaticDefinition`s (the
            // `pushed_down.is_none()` guard below), and `BranchOtherwise`'s
            // backward scan (`d.condition.is_some()`) intentionally skips those
            // nodes. Reading the def mirrors that truth exactly; reading the IR
            // would bind to a node the current scan does not.
            if def.condition.is_some() {
                self.last_conditional = Some(node);
                self.conditional_nodes.push(index);
            }
            if def.optional_for.is_some() {
                self.last_optional_for = Some(node);
                self.optional_head_nodes.push(index);
            }
            // A search-destination `ChangeZone` is only a `ContinuationProduct`
            // antecedent when a continuation actually pushed it — that is the whole
            // point of carrying provenance (audit §2). A clause that happens to
            // emit the same effect shape is NOT this antecedent.
            if matches!(
                &*def.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Hand,
                    ..
                }
            ) && provenance.role == NodeRole::ContinuationProduct
            {
                self.search_destination_nodes.push(index);
            }
            if matches!(
                &*def.effect,
                Effect::Dig { .. } | Effect::RevealUntil { .. }
            ) {
                self.dig_or_reveal_until_nodes.push(index);
            }
            // CR 608.2c: include delayed-trigger-wrapped Destroy/DestroyAll
            // (Merieke Ri Berit) in the can't-be-regenerated antecedent set, not
            // just top-level ones — descends via effect_wraps_destroy_like.
            if super::sequence::effect_wraps_destroy_like(&def.effect) {
                self.destroy_like_nodes.push(index);
            }
            if matches!(
                &*def.effect,
                Effect::ChangeZoneAll {
                    face_down_profile: Some(_),
                    library_position: None,
                    random_order: false,
                    ..
                } | Effect::ChangeZone {
                    face_down_profile: Some(_),
                    ..
                } | Effect::Manifest {
                    profile: Some(_),
                    ..
                } | Effect::TurnFaceDown {
                    profile: Some(_),
                    ..
                }
            ) {
                self.face_down_profile_nodes.push(index);
            }
            match &*def.effect {
                Effect::Dig { .. } | Effect::Mill { .. } | Effect::RevealUntil { .. } => {
                    self.last_dig = Some(node);
                }
                Effect::Destroy { .. } | Effect::DestroyAll { .. } => {
                    self.last_destroy_like = Some(node);
                }
                Effect::CastFromZone { .. } => self.last_cast_from_zone = Some(node),
                Effect::Mana { .. } => self.last_mana = Some(node),
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile { .. },
                    ..
                } => self.last_play_from_exile = Some(node),
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Hand,
                    ..
                } => self.last_search_destination = Some(node),
                _ => {}
            }
        }
    }

    /// The single authority for antecedent binding (U6-C2). Returns the bound
    /// node's CURRENT index in `defs`, or `None` on a miss.
    ///
    /// Guards are evaluated against the bound node only. Role selectors walk back
    /// over a typed candidate LIST (not the output tree) when a candidate fails
    /// its guard — which is what `BranchOtherwise`'s fallback scan does today.
    ///
    /// `on_miss` is taken explicitly and has no default: a miss must be a
    /// conscious choice, because two handlers rely on it being SILENT.
    pub(super) fn resolve(
        &self,
        defs: &[AbilityDefinition],
        selector: AntecedentSelector,
        guard: Option<BindGuard>,
        on_miss: OnMiss,
    ) -> Option<usize> {
        debug_assert!(
            !matches!(selector, AntecedentSelector::AllWithRole(_)),
            "`AllWithRole` is a FAN-OUT selector — bind it with `resolve_all`. Taking one \
             node from it would silently drop every other member, which is the exact \
             CR 707.10c defect the selector exists to fix."
        );
        let hit = self.point_hit(defs, selector, guard);
        match hit {
            Some(i) => Some(i),
            None => match on_miss {
                OnMiss::Ignore => None,
            },
        }
    }

    /// The FAN-OUT counterpart of [`Self::resolve`] — every bound node's CURRENT
    /// index in `defs`, in emission order. Empty on a miss.
    ///
    /// Only [`AntecedentSelector::AllWithRole`] can bind more than one node; every
    /// other selector is a point binding and yields 0 or 1 index here, with exactly
    /// the semantics it has in `resolve` (including `Between`'s forward-first walk).
    /// Both entry points therefore share ONE guard evaluator and ONE membership
    /// authority, so a fan-out can never disagree with the point binding about which
    /// nodes qualify.
    pub(super) fn resolve_all(
        &self,
        defs: &[AbilityDefinition],
        selector: AntecedentSelector,
        guard: Option<BindGuard>,
        on_miss: OnMiss,
    ) -> Vec<usize> {
        let hits: Vec<usize> = match selector {
            AntecedentSelector::AllWithRole(role) => self.role_members(defs, role, guard),
            point => self.point_hit(defs, point, guard).into_iter().collect(),
        };
        if hits.is_empty() {
            return match on_miss {
                OnMiss::Ignore => Vec::new(),
            };
        }
        hits
    }

    /// Does the node at `index` satisfy the binding guard? Guards are evaluated
    /// against the bound node ALONE — never by walking the output tree.
    fn guard_passes(
        &self,
        defs: &[AbilityDefinition],
        index: usize,
        guard: Option<BindGuard>,
    ) -> bool {
        match guard {
            None => true,
            Some(BindGuard::NoSubAbility) => defs[index].sub_ability.is_none(),
            Some(BindGuard::EffectShape(EffectClass::PermanentCreator)) => matches!(
                &*defs[index].effect,
                Effect::CopyTokenOf { .. }
                    | Effect::Token { .. }
                    | Effect::ChangeZone { .. }
                    | Effect::Meld { .. }
            ),
            Some(BindGuard::EffectShape(EffectClass::DrawnThisTurnChoice)) => matches!(
                &*defs[index].effect,
                Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
            ),
            Some(BindGuard::DigLookbackTransparentCost) => {
                let d = &defs[index];
                d.optional
                    && super::sequence::clause_is_dig_lookback_transparent(&d.effect)
                    && matches!(
                        &*d.effect,
                        Effect::Sacrifice { .. } | Effect::PayCost { .. }
                    )
            }
        }
    }

    /// EVERY index carrying `role` whose guard also holds, in emission order.
    ///
    /// The single membership authority. `role_members(..).last()` is by construction
    /// what `LastWithRole` binds, so the point selector and the fan-out cannot drift:
    /// the live-predicate / cached-registry split below is the one in
    /// `live_role_predicate`, not a copy of it.
    fn role_members(
        &self,
        defs: &[AbilityDefinition],
        role: AntecedentRole,
        guard: Option<BindGuard>,
    ) -> Vec<usize> {
        let members: Vec<usize> = match live_role_predicate(role) {
            Some(is_member) => (0..defs.len()).filter(|i| is_member(&defs[*i])).collect(),
            None => match role {
                AntecedentRole::Conditional => self.conditional_nodes.clone(),
                AntecedentRole::OptionalHead => self.optional_head_nodes.clone(),
                AntecedentRole::DigOrRevealUntil => self.dig_or_reveal_until_nodes.clone(),
                AntecedentRole::DestroyLike => self.destroy_like_nodes.clone(),
                AntecedentRole::FaceDownProfileHolder => self.face_down_profile_nodes.clone(),
                // `live_role_predicate` returned `Some` for these, so this arm is
                // unreachable — but it is spelled out rather than wildcarded so a
                // NEW role cannot be added without choosing a side.
                AntecedentRole::GenericEffectHead
                | AntecedentRole::KeywordCounterPlacement
                | AntecedentRole::DigOrMill
                | AntecedentRole::DigLook
                | AntecedentRole::DamageDealer
                | AntecedentRole::CopySpellBearer => Vec::new(),
            },
        };
        members
            .into_iter()
            .filter(|i| self.guard_passes(defs, *i, guard))
            .collect()
    }

    /// The single node a POINT selector binds, or `None`. Shared by `resolve` and
    /// `resolve_all`; `AllWithRole` is not a point selector and never reaches here.
    fn point_hit(
        &self,
        defs: &[AbilityDefinition],
        selector: AntecedentSelector,
        guard: Option<BindGuard>,
    ) -> Option<usize> {
        let passes = |index: usize| -> bool { self.guard_passes(defs, index, guard) };
        // The most recent candidate from a `refresh`-maintained emission-ordered list.
        let last_cached =
            |list: &[usize]| -> Option<usize> { list.iter().rev().copied().find(|i| passes(*i)) };

        match selector {
            AntecedentSelector::LastEmitted => defs.len().checked_sub(1).filter(|i| passes(*i)),
            AntecedentSelector::FirstEmitted => {
                (!defs.is_empty()).then_some(0).filter(|i| passes(*i))
            }
            AntecedentSelector::ContinuationProduct(ContinuationRole::SearchDestination) => {
                last_cached(&self.search_destination_nodes)
            }
            AntecedentSelector::Between { anchor } => {
                // Walk FORWARD over emission order from the anchor and take the
                // FIRST qualifying node. Reads `order` only — never the output tree.
                let anchor_pos = self.arena.order.iter().position(|id| *id == anchor);
                anchor_pos.and_then(|a| ((a + 1)..defs.len()).find(|i| passes(*i)))
            }
            // "The most recent node with the role" IS the last member of the same
            // membership set the fan-out enumerates — one authority, two arities.
            AntecedentSelector::LastWithRole(role) => {
                self.role_members(defs, role, guard).last().copied()
            }
            // Unreachable: `resolve` debug_asserts it away and `resolve_all` handles
            // it before calling here. Spelled out so the match stays exhaustive.
            AntecedentSelector::AllWithRole(_) => None,
        }
    }
}

pub(crate) fn assemble_effect_chain(ir: &EffectChainIr) -> AbilityDefinition {
    let kind = ir.kind;
    let continuation_kind = ir.continuation_kind.unwrap_or(AbilityKind::Spell);

    // ── Phase 1: ClauseIr → AbilityDefinition ──────────────────────────
    let mut defs: Vec<AbilityDefinition> = Vec::new();
    // U6-B1: emit-time provenance + role registries. Populated below, consumed by
    // nothing yet (U6-C). `observe` is called after every statement that changes
    // `defs.len()` — a pop followed by a push inside one region would otherwise
    // let the new node silently inherit the popped node's provenance.
    let mut env = AssemblyEnv::default();
    // CR 608.2c: Boundary that followed the previous normal-path clause. Used to
    // stamp each clause's `sub_link` — a `Sentence` boundary before this clause
    // makes it a `SequentialSibling` (independent following instruction); a
    // `Comma`/`Then`/no boundary makes it a within-clause `ContinuationStep`.
    let mut prev_boundary: Option<ClauseBoundary> = None;
    for clause_ir in &ir.clauses {
        // "The arena mirrors `defs`" is load-bearing for every binding below, and it
        // is asserted HERE, at the one point every clause path must pass through.
        //
        // `settle` runs at the end of the normal and special paths, but the five
        // rider `continue`s below skip it. Those are safe today only because their
        // helpers fold into a PRIOR def without changing `defs.len()` — a fact
        // nothing enforced. Asserting at the top of the loop enforces it for every
        // path, including any rider added later: a clause that mutates `defs` without
        // telling the arena is caught on the NEXT clause rather than silently
        // self-healed by `sync_len` (which would push a fresh node carrying the wrong
        // provenance and hand it to role membership).
        env.arena.assert_mirrors(&defs);

        // CR 608.2c: Handle absorbed clauses and special (rider) clauses that
        // modify previous defs rather than emitting new sibling defs. Each path
        // evaluates to `true`; the boundary advance below then runs uniformly so
        // a following normal clause stamps `sub_link` from the correct boundary.
        let handled_as_special: bool = {
            if matches!(clause_ir.disposition, ClauseDisposition::Continue { .. }) {
                // Apply the Continue clause's continuation to the defs built so far
                // (formerly the absorbed `followup_continuation` path).
                if let Some(continuation) = clause_ir.disposition.followup() {
                    apply_clause_continuation(&mut defs, continuation.clone(), kind, &env);
                    env.observe(&defs, None, NodeRole::ContinuationProduct);
                    apply_where_x_to_latest_def(&mut defs, clause_ir.where_x_expression.as_deref());
                }
                true
            } else if let ClauseDisposition::Absorb { rider, kind } = &clause_ir.disposition {
                // CR 614.1a / CR 701.19c: attach the rider as the tail of the prior
                // def's sub_ability chain instead of overwriting it — multi-target
                // damage spells (Serpentine Spike) populate the chain with
                // continuation events, so the rider must attach AFTER them.
                if let Some(last_def) = defs.last_mut() {
                    append_to_deepest_sub_ability(last_def, Some(rider.clone()));
                }
                // CR 608.2c: a die-exile rider printed after an optional
                // "instead" damage clause is independent of that choice.
                // The bargained branch reaches the appended tail; the
                // unbargained branch needs the same tail in else_ability.
                // The override and its Scry continuation can still be
                // separate top-level defs at this assembly stage.
                if matches!(kind, AbsorbKind::DieExile) {
                    'find_override: for root in defs.iter_mut().rev() {
                        let mut cursor = Some(root);
                        while let Some(def) = cursor {
                            if matches!(
                                def.condition,
                                Some(AbilityCondition::AdditionalCostPaidInstead)
                            ) {
                                if let Some(base_chain) = def.else_ability.as_mut() {
                                    append_to_deepest_sub_ability(base_chain, Some(rider.clone()));
                                } else {
                                    def.else_ability = Some(rider.clone());
                                }
                                break 'find_override;
                            }
                            cursor = def.sub_ability.as_deref_mut();
                        }
                    }
                }
                true
            } else if let ClauseDisposition::BranchOtherwise {
                else_def,
                kind: otherwise_kind,
            } = &clause_ir.disposition
            {
                // CR 608.2c: `otherwise_kind` was determined at PARSE time (whether
                // a prior conditional / opponent-may head existed) — it is NOT
                // recomputed here, so parse-time and lower-time views cannot diverge.
                match otherwise_kind {
                    OtherwiseKind::Bound => {
                        // U6-C2: bind "the most recent conditional def" by ROLE.
                        // Registered where `def.condition` is ACTUALLY set, so the
                        // GenericEffect static-pushdown nodes are skipped exactly as
                        // the old backward scan skipped them.
                        let bound = env.resolve(
                            &defs,
                            AntecedentSelector::LastWithRole(AntecedentRole::Conditional),
                            None,
                            OnMiss::Ignore,
                        );
                        let mut attached = false;
                        if let Some(bound_index) = bound {
                            let d = &mut defs[bound_index];
                            {
                                let mut else_def = else_def.clone();
                                // CR 608.2c: when the gated clause acts on the
                                // source (`SelfRef`), the else clause's "it" anaphor
                                // is the same source — rebind its `ParentTarget`
                                // default to `SelfRef` so a self-targeting ability's
                                // else branch is not a no-op against an empty target
                                // list (Repeat Offender's "Otherwise, suspect it").
                                if definition_targets_self_source(d) {
                                    rewrite_else_parent_target_to_self_ref(&mut else_def);
                                }
                                // CR 608.2c: bind the else branch's "that much"
                                // anaphor (`EventContextAmount`) to the if branch's
                                // stable magnitude. The if branch's amount is the
                                // same printed quantity "that much" refers to; on
                                // the else branch the antecedent instruction was
                                // skipped, so the per-instruction `EventContextAmount`
                                // channel reads 0 (Caustic Bronco: "each opponent
                                // loses that much life"). Only a stable antecedent
                                // amount (object-/fixed-bound, not itself
                                // `EventContextAmount`) is propagated.
                                if let Some(stable) = d
                                    .effect
                                    .count_expr()
                                    .filter(|e| is_stable_branch_amount(e))
                                    .cloned()
                                {
                                    rewrite_else_event_context_to_stable(&mut else_def, &stable);
                                }
                                d.else_ability = Some(else_def);
                                attached = true;
                            }
                        }
                        // CR 608.2d + CR 101.4: standalone "If no one does, X" on
                        // an "any opponent/player may" head (Browbeat, Book
                        // Burning). The head has no `condition` — it is made
                        // conditional by `optional_for`. The reward is the
                        // no-one-accepted branch. Synthesize a
                        // `Not(OptionalEffectPerformed)`-gated sub carrying the
                        // reward: on accept the head's chain skips it (signal
                        // true → negated false); on all-decline the runtime
                        // decline path fires it (see `handle_opponent_may_choice`).
                        if !attached {
                            // U6-C2: the guard is the point here — the nearest optional
                            // head may ALREADY have a sub_ability (mutable output state),
                            // in which case the old scan walked PAST it. The role list is
                            // walked back with `NoSubAbility` applied to each candidate;
                            // no output-tree inspection.
                            let bound = env.resolve(
                                &defs,
                                AntecedentSelector::LastWithRole(AntecedentRole::OptionalHead),
                                Some(BindGuard::NoSubAbility),
                                OnMiss::Ignore,
                            );
                            if let Some(bound_index) = bound {
                                let d = &mut defs[bound_index];
                                let mut reward = (**else_def).clone();
                                reward.condition = Some(AbilityCondition::Not {
                                    condition: Box::new(AbilityCondition::effect_performed()),
                                });
                                d.sub_ability = Some(Box::new(reward));
                            }
                        }
                    }
                    OtherwiseKind::Fallback => {
                        defs.push(AbilityDefinition::new(
                            kind,
                            // allow-noncombinator: pre-existing literal relocated verbatim from lower.rs (U6-A byte-identical move); conversion deferred
                            Effect::Unimplemented {
                                name: "otherwise".to_string(),
                                description: Some("Otherwise".to_string()),
                            },
                        ));
                        defs.push(*else_def.clone());
                        env.observe(&defs, Some(clause_ir.id), NodeRole::HandlerProduct);
                    }
                }
                true
            } else if let ClauseDisposition::ReplicatePerKeyword { keywords, kind } =
                &clause_ir.disposition
            {
                // CR 702 / CR 608.2c: replicate the antecedent template clause once
                // per listed keyword. `kind` selects which template shape (and thus
                // helper); the keyword-swap logic lives unchanged in the helpers.
                //
                // U6-C5: both templates are bound by ROLE. These two sites were the
                // last `defs.iter().rev()` scans that no earlier increment had even
                // noticed — they never recursed into a sub-tree, so they are plain
                // "most recent node of role X" bindings and needed no new machinery.
                match kind {
                    ReplicateKind::StaticGrant => {
                        let bound = env.resolve(
                            &defs,
                            AntecedentSelector::LastWithRole(AntecedentRole::GenericEffectHead),
                            None,
                            OnMiss::Ignore,
                        );
                        if let Some(bound_index) = bound {
                            attach_same_is_true_keywords(&mut defs[bound_index], keywords);
                        }
                    }
                    ReplicateKind::CounterPlacement => {
                        let bound = env.resolve(
                            &defs,
                            AntecedentSelector::LastWithRole(
                                AntecedentRole::KeywordCounterPlacement,
                            ),
                            None,
                            OnMiss::Ignore,
                        );
                        if let Some(bound_index) = bound {
                            attach_repeat_process_keywords(&mut defs, bound_index, keywords);
                        }
                    }
                }
                env.observe(&defs, Some(clause_ir.id), NodeRole::HandlerProduct);
                true
            } else if let ClauseDisposition::ModifyPrior { modifier } = &clause_ir.disposition {
                // CR 608.2c: fold a field-level modification onto the prior emitted
                // def; emit no sibling. `modifier` selects which field/aspect.
                match modifier {
                    PriorModifier::AltCost(cost) => {
                        attach_alt_cost_to_prior_cast_from_zone(&mut defs, cost.clone());
                    }
                    PriorModifier::ManaRetention(expiry) => {
                        attach_mana_retention_to_prior_mana(&mut defs, *expiry);
                    }
                    PriorModifier::EntersTappedAttacking => {
                        // CR 508.4 / CR 614.1: Conditional enters-tapped-attacking modifier.
                        // U6-C2: LastEmitted + an EffectShape guard. A wrong-shaped prior
                        // def is a MISS, and `OnMiss::Ignore` makes it a silent no-op —
                        // identical to the old inline `can_patch` check with no else arm.
                        let bound = env.resolve(
                            &defs,
                            AntecedentSelector::LastEmitted,
                            Some(BindGuard::EffectShape(EffectClass::PermanentCreator)),
                            OnMiss::Ignore,
                        );
                        if bound.is_some() {
                            {
                                // Identity: `patched` IS the popped def, mutated — a MOVE,
                                // not a creation. It must keep its NodeId.
                                let patched_id = env.arena.id_at(defs.len() - 1);
                                let mut patched = defs.pop().unwrap();
                                env.observe(&defs, None, NodeRole::Unknown);
                                match &mut *patched.effect {
                                    Effect::CopyTokenOf {
                                        enters_attacking,
                                        tapped,
                                        ..
                                    } => {
                                        *enters_attacking = true;
                                        *tapped = true;
                                    }
                                    Effect::Token {
                                        enters_attacking,
                                        tapped,
                                        ..
                                    } => {
                                        *enters_attacking = true;
                                        *tapped = true;
                                    }
                                    Effect::ChangeZone {
                                        enters_attacking,
                                        enter_tapped,
                                        ..
                                    } => {
                                        *enters_attacking = true;
                                        *enter_tapped = crate::types::zones::EtbTapState::Tapped;
                                    }
                                    Effect::Meld { entry, .. } => {
                                        *entry = crate::types::ability::PermanentEntryMode::TappedAndAttacking {
                                            destination: crate::types::ability::EntryAttackDestination::AnyDefender,
                                        };
                                    }
                                    _ => {}
                                }
                                let original = {
                                    let mut orig = patched.clone();
                                    match &mut *orig.effect {
                                        Effect::CopyTokenOf {
                                            enters_attacking,
                                            tapped,
                                            ..
                                        } => {
                                            *enters_attacking = false;
                                            *tapped = false;
                                        }
                                        Effect::Token {
                                            enters_attacking,
                                            tapped,
                                            ..
                                        } => {
                                            *enters_attacking = false;
                                            *tapped = false;
                                        }
                                        Effect::ChangeZone {
                                            enters_attacking,
                                            enter_tapped,
                                            ..
                                        } => {
                                            *enters_attacking = false;
                                            *enter_tapped =
                                                crate::types::zones::EtbTapState::Unspecified;
                                        }
                                        Effect::Meld { entry, .. } => {
                                            *entry =
                                                crate::types::ability::PermanentEntryMode::Normal;
                                        }
                                        _ => {}
                                    }
                                    orig
                                };
                                patched.condition = clause_ir.condition.clone();
                                patched.else_ability = Some(Box::new(original));
                                defs.push(patched);
                                if let Some(id) = patched_id {
                                    env.arena.reinstate(id);
                                }
                                env.observe(&defs, Some(clause_ir.id), NodeRole::HandlerProduct);
                            }
                        }
                    }
                }
                true
            } else if let ClauseDisposition::ReplaceMeaning { kind: replace_kind } =
                &clause_ir.disposition
            {
                // CR 608.2c / CR 614.1a: replace/override the meaning of the prior
                // emitted def(s); `kind` selects which (moved verbatim from the old
                // special-clause arms).
                match replace_kind {
                    ReplaceMeaningKind::DigAlt(alt_def) => {
                        // Identity: `new_def` genuinely IS new (it comes from the IR),
                        // so it correctly gets a fresh id — this is the one absorbing
                        // handler that does NOT `reinstate`. The popped def is nested
                        // under it as `else_ability`, so it names that parent.
                        let absorbed = defs
                            .len()
                            .checked_sub(1)
                            .and_then(|last| env.arena.id_at(last));
                        if let Some(last_def) = defs.pop() {
                            env.observe(&defs, None, NodeRole::Unknown);
                            let mut new_def = *alt_def.clone();
                            apply_where_x_ability_expression(
                                &mut new_def,
                                clause_ir.where_x_expression.as_deref(),
                            );
                            new_def.else_ability = Some(Box::new(last_def));
                            defs.push(new_def);
                            env.observe(&defs, Some(clause_ir.id), NodeRole::HandlerProduct);
                            let parent = env
                                .arena
                                .id_at(defs.len() - 1)
                                .expect("the replacement def was just pushed");
                            if let Some(absorbed) = absorbed {
                                env.arena.absorb(absorbed, parent);
                            }
                        }
                        true
                    }
                    ReplaceMeaningKind::Instead(instead_def) => {
                        // CR 614.1a + CR 608.2c: assemble a multi-clause base + an
                        // "instead" override so the runtime can produce both
                        // branches. Clause 1 becomes the root and is the Cow-swap
                        // target — when the override's `ConditionInstead` fires,
                        // `effects/mod.rs` swaps the root's effect with the
                        // override's at parent resolution, and the override branch
                        // returns terminally (see the `ConditionInstead` arm at
                        // ~line 2713 in `effects/mod.rs`). To make the tail clauses
                        // (2..N) conditional on the override NOT firing, we stash
                        // them in the override's `else_ability`: the runtime only
                        // walks `else_ability` when the swap did not happen. Net:
                        // condition true → only the override's effect runs (clause
                        // 1 swapped away, tail bypassed); condition false → clause
                        // 1 runs as printed, then the tail runs from
                        // `else_ability`. Single-clause bases collapse to the
                        // prior shape (empty tail → no `else_ability`).
                        // U6-C2: `Instead` is the ONLY handler that binds FirstEmitted
                        // (CR 608.2c — the override replaces the FIRST printed
                        // instruction). Do not unify it with the `Last*` selectors.
                        let bound = env.resolve(
                            &defs,
                            AntecedentSelector::FirstEmitted,
                            None,
                            OnMiss::Ignore,
                        );
                        if bound.is_some() {
                            // Identity: the root is MOVED out of `defs` and back in —
                            // it keeps its NodeId (U6-C2 ruling). Capture before the take.
                            // The TAIL defs (1..N) are nested into the root below, so
                            // capture their ids too and name the root as their parent —
                            // `settle` no longer infers one.
                            let root_id = env.arena.id_at(0);
                            let tail_ids = env.arena.tail_ids();
                            let mut chain_defs = std::mem::take(&mut defs);
                            env.observe(&defs, None, NodeRole::Unknown);
                            let mut root = chain_defs.remove(0);
                            for next in chain_defs {
                                append_to_deepest_sub_ability(&mut root, Some(Box::new(next)));
                            }
                            let mut instead = *instead_def.clone();
                            // CR 702.33d + CR 707.10: Resolve "create N of those
                            // tokens" anaphor against the root (the antecedent
                            // for a multi-clause base is the first printed clause).
                            rewrite_those_tokens_from_antecedent(&mut instead.effect, &root.effect);
                            if rewrite_counter_instead_target_from_antecedent(
                                &mut instead.effect,
                                &root.effect,
                            ) {
                                instead.target_choice_timing = root.target_choice_timing;
                            }
                            if has_explicit_player_target(root.effect.as_ref()) {
                                rewrite_player_anaphor_targets_in_definition(&mut instead);
                            }
                            instead.else_ability = root.sub_ability.take();
                            root.sub_ability = Some(Box::new(instead));
                            defs.push(root);
                            if let Some(id) = root_id {
                                env.arena.reinstate(id);
                                // Each tail def was appended into the root's sub-spine
                                // by `append_to_deepest_sub_ability`.
                                for tail in tail_ids {
                                    env.arena.absorb(tail, id);
                                }
                            }
                            env.observe(&defs, Some(clause_ir.id), NodeRole::HandlerProduct);
                        }
                        true
                    }
                    ReplaceMeaningKind::KeywordOverride => {
                        // Build the def for this clause and attach to previous as sub_ability
                        let mut def = AbilityDefinition::new(kind, clause_ir.parsed.effect.clone());
                        let effective_cond = clause_ir
                            .condition
                            .as_ref()
                            .or(clause_ir.parsed.condition.as_ref());
                        if let Some(cond) = effective_cond {
                            def = def.condition(cond.clone());
                        }
                        if let Some(prev) = defs.last_mut() {
                            prev.sub_ability = Some(Box::new(def));
                        }
                        true
                    }
                }
            } else if let ClauseDisposition::FoldSearchIntoElse { intrinsic } =
                &clause_ir.disposition
            {
                // CR 608.2c + CR 601.2b: later text ("if this spell was kicked, instead
                // search …") modifies the meaning of the earlier search, gated on an
                // additional cost announced at cast. Build this clause's def and fold
                // else_ability from the trailing clause. The trailing ChangeZone was
                // produced by the previous SearchLibrary's intrinsic continuation
                // (SearchDestination).
                let mut def = AbilityDefinition::new(kind, clause_ir.parsed.effect.clone());
                let effective_cond = clause_ir
                    .condition
                    .as_ref()
                    .or(clause_ir.parsed.condition.as_ref());
                if let Some(cond) = effective_cond {
                    def = def.condition(cond.clone());
                }
                // U6-C2: bind the PRIOR search's continuation-pushed destination node by
                // PROVENANCE instead of sniffing the tail's effect shape. This is the
                // binding the whole `origin: Option<ClauseId>` redesign existed for: the
                // node belongs to no clause, so only role+provenance can name it. The
                // positional `defs.len() >= 2` guard disappears — the binding either
                // resolves or it does not.
                let bound = env.resolve(
                    &defs,
                    AntecedentSelector::ContinuationProduct(ContinuationRole::SearchDestination),
                    None,
                    OnMiss::Ignore,
                );
                let absorbed = if let Some(bound_index) = bound {
                    // `bound_index` is wherever the destination node sits — the selector
                    // is `last_cached(&search_destination_nodes)`, which is under no
                    // obligation to return the tail. So this is a MID-VECTOR removal and
                    // is announced as one: `observe`/`sync_len` can only mirror a TAIL
                    // shrink, and would otherwise detach the LAST node while `defs`
                    // dropped THIS one, renaming every node from here on.
                    //
                    // Measured: on the full ~35k-face pool this fold fires twice, and
                    // both are currently tail removals — so the tail-pop mirror happened
                    // to agree, and the divergence is latent rather than live. It is
                    // fixed structurally anyway: correctness here must not rest on a
                    // coincidence about today's card pool.
                    let id = env.arena.remove_at(bound_index);
                    def.else_ability = Some(Box::new(defs.remove(bound_index)));
                    env.observe(&defs, None, NodeRole::Unknown);
                    Some(id)
                } else {
                    None
                };
                defs.push(def);
                env.observe(&defs, Some(clause_ir.id), NodeRole::HandlerProduct);
                if let Some(absorbed) = absorbed {
                    // Name the parent EXPLICITLY. `settle` would infer it as
                    // `order.last()`, but the intrinsic continuation below pushes the
                    // NEW search-destination node AFTER this def — so the inference
                    // names the continuation's node as parent of a def that is in fact
                    // nested in THIS def's `else_ability`.
                    let parent = env
                        .arena
                        .id_at(defs.len() - 1)
                        .expect("the def carrying the folded `else_ability` was just pushed");
                    env.arena.absorb(absorbed, parent);
                }
                // Apply intrinsic continuation for THIS SearchLibrary (e.g., reveal flag, ChangeZone).
                if let Some(continuation) = intrinsic {
                    apply_clause_continuation(&mut defs, continuation.clone(), kind, &env);
                    env.observe(&defs, None, NodeRole::ContinuationProduct);
                }
                true
            } else if let ClauseDisposition::DrawnThisTurnFollowup { life_payment } =
                &clause_ir.disposition
            {
                // Set the life payment on the prior drawn-this-turn choice; emits no def.
                // U6-C2: LastEmitted + EffectShape guard. A wrong-shaped prior def is a
                // MISS and `OnMiss::Ignore` keeps it a SILENT no-op — the old code did
                // the same via a nested `if let` with no else arm.
                let bound = env.resolve(
                    &defs,
                    AntecedentSelector::LastEmitted,
                    Some(BindGuard::EffectShape(EffectClass::DrawnThisTurnChoice)),
                    OnMiss::Ignore,
                );
                if let Some(bound_index) = bound {
                    if let Effect::ChooseDrawnThisTurnPayOrTopdeck {
                        life_payment: current,
                        ..
                    } = &mut *defs[bound_index].effect
                    {
                        *current = life_payment.clone();
                    }
                }
                true
            } else {
                false
            }
        };

        // CR 608.2c: A special/absorbed clause emits no sibling def, but it
        // still occupies a slot in the chunk sequence and carries its own
        // trailing `boundary` (`ClauseIr.boundary` is populated from
        // `ClauseChunk.boundary_after`). Advance `prev_boundary` so a following
        // normal clause stamps its `sub_link` from the boundary AFTER this
        // clause, not the stale boundary that preceded it.
        if handled_as_special {
            prev_boundary = clause_ir.boundary;
            // Classify anything this handler detached; the mirror is asserted at the
            // next loop-top, or at the Phase-1/Phase-2 boundary for the last clause.
            env.arena.settle();
            continue;
        }

        // CR 609.4b + CR 608.2c: Brainstealer/Daxos-class any-color mana
        // riders may be split into their own sentence or comma sibling after a
        // `PlayFromExile` grant. They scope the existing exile-play
        // permission, so fold the rider into the prior grant instead of
        // emitting a broad standalone `SpendManaAsAnyColor` effect.
        if is_spend_mana_as_any_color_rider(clause_ir)
            && attach_any_color_mana_rider_to_previous_play_from_exile(&mut defs)
        {
            prev_boundary = clause_ir.boundary;
            continue;
        }

        // CR 614.1a + CR 608.2n: a "if that spell would be put into a graveyard,
        // [put on library / return to hand] instead" rider that trails an
        // optional `CastFromZone` (Kylox's Voltstrider) is a CR 608.2n
        // destination-replacement on the cast spell. Fold the canonical rider
        // onto the prior cast so the runtime stamps the redirect, intercepting it
        // before the generic chain assembly mistakes a `PutAtLibraryPosition{
        // Bottom}` for the Sanwell free-cast bottom-cleanup. Exile is left to its
        // existing clean path (the helper declines it).
        if let Some(dest) = parse_spell_graveyard_replacement_rider(
            &clause_ir
                .source
                .fragment()
                .unwrap_or_default()
                .to_lowercase(),
        ) {
            if attach_graveyard_redirect_rider_to_prior_cast_from_zone(&mut defs, dest) {
                prev_boundary = clause_ir.boundary;
                continue;
            }
        }

        // CR 601.2f + CR 614.1c: Lightstall Inquisitor's "Each spell cast this
        // way costs {1} more to cast." / "Each land played this way enters
        // tapped." rider sentences scope to the preceding `PlayFromExile`
        // grant. Fold each into the grant (`cast_cost_raise` /
        // `land_enter_tapped`) instead of emitting a standalone cost-modify
        // static or board-wide ETB-tapped replacement — "this way" binds them
        // to the exile-play permission, not to all spells/lands.
        if let Some(cost) = cast_cost_raise_rider(clause_ir) {
            if attach_cast_cost_raise_to_previous_play_from_exile(&mut defs, cost) {
                prev_boundary = clause_ir.boundary;
                continue;
            }
        }
        if is_land_enters_tapped_rider(clause_ir)
            && attach_land_enters_tapped_to_previous_play_from_exile(&mut defs)
        {
            prev_boundary = clause_ir.boundary;
            continue;
        }

        // Non-absorbed, non-special followup continuation — apply it to the
        // previous defs before building this clause's def (`Emit.followup`).
        if let Some(continuation) = clause_ir.disposition.followup() {
            apply_clause_continuation(&mut defs, continuation.clone(), kind, &env);
            env.observe(&defs, None, NodeRole::ContinuationProduct);
            apply_where_x_to_latest_def(&mut defs, clause_ir.where_x_expression.as_deref());
        }

        // ── Build AbilityDefinition from ClauseIr ──
        let is_target_only = matches!(clause_ir.parsed.effect, Effect::TargetOnly { .. });
        let mut def = AbilityDefinition::new(kind, clause_ir.parsed.effect.clone());
        // CR 702.26a: Preserve clause provenance on parent-target tap riders so
        // host-bound phase-in rewrites can match the exact printed phrase without
        // falling back to whole-trigger text.
        if matches!(
            def.effect.as_ref(),
            Effect::SetTapState {
                state: TapStateChange::Tap,
                target: TargetFilter::ParentTarget,
                ..
            }
        ) {
            def.description = Some(clause_ir.source.fragment().unwrap_or_default().to_string());
        }
        // CR 608.2c: This clause's link to its parent = the boundary that
        // SEPARATED the previous clause from this one. A `Sentence` boundary
        // marks a `SequentialSibling` (next printed instruction, resolves even
        // when an optional parent is declined); `Comma`/`Then`/none marks a
        // within-clause `ContinuationStep` (part of the parent's action).
        def.sub_link = match prev_boundary {
            Some(ClauseBoundary::Sentence) => SubAbilityLink::SequentialSibling,
            Some(ClauseBoundary::Then) | Some(ClauseBoundary::Comma) | None => {
                SubAbilityLink::ContinuationStep
            }
        };
        // CR 615.5: A "(When|Whenever|If) damage [from a <type> source] is
        // prevented this way, …" rider is printed as its own sentence but is not
        // an independent instruction — its "this way" back-reference binds to the
        // prevention in the chain. Detect it (only when the chain root is that
        // prevention — `any` covers Comeuppance's TWO riders, whose second rider's
        // immediate predecessor is the first rider, not the PreventDamage) so the
        // clause is folded into the prevention rather than dropped as a sibling.
        let prevented_this_way_gate = if defs
            .iter()
            .any(|d| matches!(&*d.effect, Effect::PreventDamage { .. }))
        {
            crate::parser::oracle_replacement::prevented_this_way_rider_source_gate(
                clause_ir.source.fragment().unwrap_or_default(),
            )
        } else {
            None
        };
        let is_prevented_this_way_rider = prevented_this_way_gate.is_some();
        // The Sentence boundary would mark the rider `SequentialSibling`, which the
        // prevention resolver never installs as the shield's `runtime_execute` (the
        // payoff silently does nothing — New Way Forward, Phyrexian Vindicator,
        // Outfitted Jouster). Force `ContinuationStep` so it rides the shield.
        if is_prevented_this_way_rider {
            def.sub_link = SubAbilityLink::ContinuationStep;
        }
        def.target_choice_timing = target_choice_timing_for_clause(clause_ir);
        // CR 115.1 + CR 701.9b: copy the per-clause selection mode captured by
        // `parse_target_with_ctx` during chunk parse. `Random` flips the engine
        // off the controller-choice path at target-selection time.
        def.target_selection_mode = clause_ir.target_selection_mode;
        // CR 601.2c + CR 603.3d: copy the per-clause target chooser captured by
        // `parse_target_with_ctx` during chunk parse, so a targeted "of their
        // choice" routes target selection to the scoped (upkeep) player.
        def.target_chooser = clause_ir.target_chooser.clone();
        let clause_sub = if is_target_only {
            def.sub_ability = clause_ir.parsed.sub_ability.clone();
            None
        } else {
            clause_ir.parsed.sub_ability.clone()
        };

        // CR 118.9 + CR 608.2g: A *standing-duration* `CastFromZone` lingering
        // grant (no DuringResolution driver, no alternative cost, no
        // eligibility constraint, and an explicit duration — Discover/Nashi/
        // Urza-class "until end of turn, you may play that card without
        // paying its mana cost") is unconditional, like
        // `GrantCastingPermission`: the "may" describes the later cast
        // decision, not the grant itself. Gating the grant behind an
        // immediate accept/decline drops the permission entirely when
        // declined (issue #720 follow-up: Urza, Lord High Artificer).
        // `constraint`/`alt_ability_cost` are EXCLUDED from this carve-out:
        // Beseech the Mirror's "...if that spell's mana value is 4 or less"
        // (constraint) and Infamous Cruelclaw's "...by discarding a card
        // rather than paying its mana cost" (alt_ability_cost) both need the
        // immediate accept/decline because declining branches into a
        // fallback action (hand fallback) or an alternative payment the
        // engine must resolve right now, not later. A missing `duration` is
        // ALSO excluded: Memory Plunder's "you may cast target instant or
        // sorcery card... without paying its mana cost" carries no standing
        // duration at all, so its "may" is the immediate resolution-time
        // decision the existing `OptionalEffectChoice` prompt correctly
        // drives (issue #2884) — only an explicit duration marks the grant
        // as deferred to a later priority window.
        let is_lingering_cast_from_zone = matches!(
            &clause_ir.parsed.effect,
            Effect::CastFromZone {
                driver: CastFromZoneDriver::LingeringPermission,
                constraint: None,
                alt_ability_cost: None,
                duration: Some(_),
                ..
            }
        );
        // CR 107.1b/c + CR 117.1d: Join Forces' "each player may pay any
        // amount of mana" is NOT an OptionalEffectChoice — the "may" only
        // means each player may pay zero. PayAmountChoice (min=0) handles
        // that; flagging the PayCost as optional would let a decline skip the
        // mill/draw body.
        let is_join_forces_pay_any_amount_mana_cost = clause_ir.player_scope
            == Some(PlayerFilter::All)
            && clause_ir.starting_with == Some(ControllerRef::You)
            && matches!(
                &clause_ir.parsed.effect,
                Effect::PayCost {
                    cost: AbilityCost::Mana { cost },
                    scale: None,
                    ..
                } if crate::game::casting_costs::cost_has_x(cost)
            );
        let is_pay_to_end_effect_termination =
            crate::parser::clause_shell::is_you_may_pay_to_end_effect_phrase(
                &clause_ir
                    .source
                    .fragment()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            );
        if clause_ir.is_optional
            && !matches!(&clause_ir.parsed.effect, Effect::SearchOutsideGame { .. })
            && !matches!(
                &clause_ir.parsed.effect,
                Effect::GrantCastingPermission { .. }
            )
            && !is_lingering_cast_from_zone
            && !is_join_forces_pay_any_amount_mana_cost
            && !is_pay_to_end_effect_termination
        {
            def.optional = true;
            def.optional_for = clause_ir.opponent_may_scope;
        }
        // CR 117.3a + CR 608.2c: Propagate subject-phrase "may" modal.
        if clause_ir.parsed.optional
            && !matches!(
                &clause_ir.parsed.effect,
                Effect::GrantCastingPermission { .. }
            )
            && !is_lingering_cast_from_zone
            && !is_join_forces_pay_any_amount_mana_cost
            && !is_pay_to_end_effect_termination
        {
            def.optional = true;
        }
        if matches!(&clause_ir.parsed.effect, Effect::SearchOutsideGame { .. }) {
            def.optional = false;
            def.optional_for = None;
        }
        if let Some(ref qty) = clause_ir.repeat_for {
            if matches!(*def.effect, Effect::TargetOnly { .. }) {
                if let Some(sub) = def.sub_ability.as_mut() {
                    sub.repeat_for = Some(qty.clone());
                } else {
                    def.repeat_for = Some(qty.clone());
                }
            } else if ir.clauses.len() == 1
                && clause_ir.parsed.sub_ability.is_none()
                && try_fold_token_repeat_into_count(def.effect.as_mut(), qty)
            {
                // CR 111.1 + CR 616.1: bare "for each X, create a token" folded
                // into one batched CreateToken event — no loop. Conservatively
                // restricted to a single-clause ability: a trailing sibling may
                // reference the created tokens (a tracked set or "those tokens"
                // anaphor — e.g. Ezuri's Predation's fight pairing depends on the
                // per-iteration creation), and we do not yet distinguish such
                // token-referencing siblings from independent ones (e.g. Moogles'
                // Valor's "creatures you control gain indestructible"), so we keep
                // the loop for all multi-clause cases. The chained-body guard
                // reads `clause_ir.parsed.sub_ability` (the non-TargetOnly path
                // attaches it after this point via `clause_sub`, so
                // `def.sub_ability` is not yet populated here).
            } else {
                def.repeat_for = Some(qty.clone());
            }
        }
        if let Some(scope) = clause_ir.player_scope.clone() {
            def.player_scope = Some(scope);
        }
        // CR 101.4 + CR 800.4: Stamp the turn-order override from the chunk's
        // "Starting with you, " prefix (Join Forces). The iteration site reads
        // this via `players::apnap_order_from(state, starting_with, controller)`
        // so the controller is prompted first regardless of the active player.
        if let Some(ref who) = clause_ir.starting_with {
            def.starting_with = Some(who.clone());
        }
        if let Some(ref duration) = clause_ir.parsed.duration {
            def = def.duration(duration.clone());
        }
        // CR 608.2c: Apply condition — chain-level takes priority over clause-level.
        let effective_condition = clause_ir
            .condition
            .as_ref()
            .or(clause_ir.parsed.condition.as_ref());
        // CR 608.2c + CR 109.2: When a "if it's a [type], it ..." card-type gate
        // sits on a clause whose effect acts on the parent target (the anaphoric
        // "it" resolved to the previously-targeted object — e.g. Azure Beastbinder's
        // "If it's a creature, it also has base power and toughness 2/2"), the
        // type description refers to that *permanent*, not a revealed card. The
        // chunk parser emits `RevealedHasCardType` for every "if it's a [type]"
        // head, but that variant evaluates against the last revealed/zone-changed
        // card and would be ALWAYS-FALSE here (no reveal context), silently
        // dropping the rider. Convert it to `TargetMatchesFilter` (the same
        // conversion the Disintegrate/Carbonize damage-rider path performs via
        // `card_type_condition_as_target_match`) so the gate evaluates against the
        // bound parent target.
        //
        // Two guards keep genuine reveal-context gates (Goblin Guide:
        // "defending player reveals the top card of their library. If it's a
        // land card, that player puts it into their hand."; Delver-class)
        // untouched:
        //   1. The gated effect must target `ParentTarget` (the "it" anaphor).
        //   2. No prior clause in the chain may publish a revealed/zone-changed
        //      subject — that is exactly the source `RevealedHasCardType` reads
        //      at resolution (`last_revealed_ids` / `last_zone_changed_ids`), so
        //      when such a publisher exists the "it" really is the revealed card
        //      and the original variant is correct.
        let chain_has_revealed_subject = defs
            .iter()
            .any(|d| effect_publishes_revealed_subject(&d.effect));
        let converted_condition = effective_condition.and_then(|cond| {
            (!chain_has_revealed_subject
                && matches!(def.effect.target_filter(), Some(TargetFilter::ParentTarget)))
            .then(|| super::conditions::card_type_condition_as_target_match(cond))
            .flatten()
        });
        let effective_condition = converted_condition.as_ref().or(effective_condition);
        if let Some(cond) = effective_condition {
            // CR 603.4 + CR 608.2h: An in-effect `if` on a continuous
            // keyword-grant clause (Odric, Lunarch Marshal) must gate each
            // `StaticDefinition` individually, NOT the whole ability — the
            // "the same is true for" continuation later swaps the gated
            // keyword per arm. Push the condition down onto every
            // `StaticDefinition` (as a `StaticCondition`, where `effect.rs`
            // evaluates it once at resolution) instead of onto
            // `AbilityDefinition.condition`. Falls back to the ability-level
            // condition when the effect is not a `GenericEffect` or the
            // condition is not invertible to a `StaticCondition`.
            let pushed_down = if let Effect::GenericEffect {
                static_abilities, ..
            } = &mut *def.effect
            {
                ability_condition_to_static_condition(cond).map(|static_cond| {
                    for static_def in static_abilities.iter_mut() {
                        // CR 611.3a + CR 118.12a: compose the outer/effective
                        // clause condition with any per-static condition the
                        // static parser already established, rather than dropping
                        // one. Both gates must survive to runtime (mirrors the
                        // `StaticCondition::And` composition in
                        // `oracle_static/anthem.rs`).
                        static_def.condition = Some(match static_def.condition.take() {
                            Some(existing) => StaticCondition::And {
                                conditions: vec![static_cond.clone(), existing],
                            },
                            None => static_cond.clone(),
                        });
                    }
                })
            } else {
                None
            };
            if pushed_down.is_none() {
                def = def.condition(cond.clone());
            }
        }
        // CR 115.1d: Apply multi-target spec — prefer explicit choose-count text,
        // then strip result, then clause-level propagation. An explicit
        // `ChooseObjectsIntoTrackedSet` instead owns its selection cardinality in
        // the effect's `min`/`max`; adding `multi_target` would duplicate that
        // resolver-owned selection state.
        if !matches!(&*def.effect, Effect::ChooseObjectsIntoTrackedSet { .. }) {
            if let Some(spec) =
                extract_exact_target_multi_target(clause_ir.source.fragment().unwrap_or_default())
            {
                def = def.multi_target(spec);
            } else if let Some(spec) =
                extract_bounded_target_multi_target(clause_ir.source.fragment().unwrap_or_default())
            {
                def = def.multi_target(spec);
            } else if let Some(spec) = extract_optional_target_multi_target(
                clause_ir.source.fragment().unwrap_or_default(),
            ) {
                def = def.multi_target(spec);
            } else if let Some(spec) =
                extract_verb_up_to_multi_target(clause_ir.source.fragment().unwrap_or_default())
            {
                def = def.multi_target(spec);
            } else if let Some(ref spec) = clause_ir.multi_target {
                def = def.multi_target(spec.clone());
            } else if let Some(ref spec) = clause_ir.parsed.multi_target {
                def = def.multi_target(spec.clone());
            }
        }
        if parse_controlled_by_different_players_target_constraint(
            clause_ir.source.fragment().unwrap_or_default(),
        ) {
            def = def.target_constraint(TargetSelectionConstraint::DifferentObjectControllers);
        }
        if let Some(constraint) =
            parse_same_zone_owner_target_constraint(clause_ir.source.fragment().unwrap_or_default())
        {
            def = def.target_constraint(constraint);
        }
        if let Some(constraint) = parse_total_mana_value_target_constraint(
            clause_ir.source.fragment().unwrap_or_default(),
        ) {
            def = def.target_constraint(constraint);
        }
        // CR 601.2d: Propagate distribute flag.
        if let Some(ref unit) = clause_ir.parsed.distribute {
            def = def.distribute(unit.clone());
        }
        if let Some(ref modifier) = clause_ir.unless_pay {
            def = def.unless_pay(modifier.clone());
        }

        let mut current_defs = vec![def];
        if let Some(ref sub) = clause_sub {
            current_defs.push(*sub.clone());
        }
        for current in &mut current_defs {
            apply_where_x_ability_expression(current, clause_ir.where_x_expression.as_deref());
        }

        // CR 615.5 + CR 609.7: In a "damage is prevented this way" rider, the
        // surface phrase "that source's controller" lowers to
        // `ParentTargetController`, but there is no parent-target slot at runtime —
        // it refers to the controller of the PREVENTED event's damage source (New
        // Way Forward: "deals that much damage to that source's controller").
        // Rewrite it to `PostReplacementSourceController` so the rider resolves
        // against the shield's event context, mirroring the static-shield
        // follow-up path in `oracle_replacement`.
        if is_prevented_this_way_rider {
            for current in &mut current_defs {
                crate::parser::oracle_replacement::rewrite_parent_target_controller_to_post_replacement_source(
                    current,
                );
            }
            // CR 615.5 + CR 120.1: A source-type-qualified rider ("if damage from
            // a creature source is prevented this way, …" — Comeuppance) gates the
            // reflection on the prevented event's source type and reflects to that
            // source object. Attach the gate as
            // `PostReplacementDamageSourceMatchesFilter` and rewrite the "that
            // creature"/"that source" anaphor (`TriggeringSource`) to
            // `PostReplacementDamageSource`. The bare rider (`Some(None)`) keeps
            // its existing unconditional behavior.
            if let Some(Some(gate_filter)) = &prevented_this_way_gate {
                for current in &mut current_defs {
                    crate::parser::oracle_replacement::rewrite_triggering_source_to_post_replacement_damage_source(
                        current,
                    );
                    // CR 608.2c: The reflection gate is conjoined with any
                    // co-existing rider condition, not substituted for it — a rider
                    // that already carries a game-state condition (e.g. an embedded
                    // "if you control …") must satisfy BOTH. Compose through the
                    // single-authority `merge_ability_condition` building block so
                    // the gate is never silently dropped when a condition is present.
                    let gate =
                        crate::types::ability::AbilityCondition::PostReplacementDamageSourceMatchesFilter {
                            filter: gate_filter.clone(),
                        };
                    current.condition = Some(crate::parser::oracle::merge_ability_condition(
                        current.condition.take(),
                        gate,
                    ));
                }
            }
        }

        // CR 603.7: Wrap in CreateDelayedTrigger if temporal suffix was found.
        if let Some(ref delayed_cond) = clause_ir.delayed_condition {
            for current in &mut current_defs {
                let inner = std::mem::replace(
                    current,
                    AbilityDefinition::new(
                        kind,
                        // `description: None` is load-bearing: `Effect::unimplemented()` would
                        // force `Some(..)` and change serialized output, so the conversion is
                        // deferred out of this byte-identical move.
                        // allow-noncombinator: pre-existing literal relocated verbatim from lower.rs (U6-A byte-identical move); conversion deferred
                        Effect::Unimplemented {
                            name: "placeholder".to_string(),
                            description: None,
                        },
                    ),
                );
                // CR 608.2c: Lift condition/optional/repeat/player_scope to outer wrapper.
                let lifted_condition = inner.condition.clone();
                let lifted_optional = inner.optional;
                let lifted_optional_for = inner.optional_for;
                let lifted_repeat_for = inner.repeat_for.clone();
                let lifted_player_scope = inner.player_scope.clone();
                *current = AbilityDefinition::new(
                    kind,
                    Effect::CreateDelayedTrigger {
                        condition: delayed_cond.clone(),
                        effect: Box::new(inner),
                        uses_tracked_set: false,
                    },
                );
                current.condition = lifted_condition;
                current.optional = lifted_optional;
                current.optional_for = lifted_optional_for;
                current.repeat_for = lifted_repeat_for;
                current.player_scope = lifted_player_scope;
            }
        }

        // CR 603.7: Cross-clause pronoun → mark uses_tracked_set on delayed trigger
        // and bind direct follow-up ParentTarget references to the affected set.
        if !current_defs.is_empty() {
            let source_text_lower = clause_ir
                .source
                .fragment()
                .unwrap_or_default()
                .to_lowercase();
            // CR 603.7: Scan ALL prior clauses for a tracked-set publisher — an
            // intermediate non-publishing clause (e.g. Investigate) must not
            // shadow an earlier exile clause. Example: Disorder in the Court
            // (exile → investigate → return the exiled cards).
            let any_prior_publishes = defs
                .iter()
                .any(|d| publishes_tracked_set_from_resolution(&d.effect));
            if any_prior_publishes {
                let has_tracked_ref = contains_explicit_tracked_set_pronoun(&source_text_lower)
                    || contains_implicit_tracked_set_pronoun(&source_text_lower);
                if has_tracked_ref {
                    for current in &mut current_defs {
                        mark_uses_tracked_set(current);
                        rewrite_parent_targets_to_tracked_set(&mut current.effect);
                    }
                }
            } else if contains_explicit_tracked_set_pronoun(&source_text_lower) {
                // CR 603.7 + issue #6065: "those creatures gain <keyword>" after a
                // "draw a card for each <creature filter>" clause (Inspiring Call).
                // Draw publishes no tracked set (its target is the drawing player),
                // so the branch above skips it and the grant's ParentTarget would
                // resolve to the controller. Bind it directly to the nearest prior
                // Draw's count filter — the counted creatures the pronoun names.
                if let Some(filter) = defs
                    .iter()
                    .rev()
                    .find_map(|d| draw_object_count_filter(&d.effect))
                    .cloned()
                {
                    for current in &mut current_defs {
                        rewrite_grant_parent_to_filter(&mut current.effect, &filter);
                    }
                }
            }

            // CR 608.2c: Re-anchor this clause's set-anaphor AGGREGATE ("their
            // total power", "the greatest power among them") to the set an
            // EARLIER clause published, overriding the leaf combinator's
            // context-free triggering-batch reading.
            //
            // Strictly prior by construction: `defs` holds only the clauses
            // already emitted, so a clause does not count itself as its own
            // publisher — which is what keeps Witch-king, Sky Scourge ("exile
            // the top X cards …, where X is their total power", whose own
            // `ExileTop` IS a publisher) on the triggering-batch reading while
            // Kylox, Visionary Inventor (a preceding SACRIFICE clause) flips to
            // the chain set.
            //
            // A separate scan from `any_prior_publishes` above, on its own
            // predicate: this axis additionally counts `Effect::Sacrifice` as a
            // producer, and it must not widen the pronoun→target rewrite that
            // the gate above drives. See
            // `publishes_aggregate_set_from_resolution`.
            if defs
                .iter()
                .any(|d| publishes_aggregate_set_from_resolution(&d.effect))
            {
                for current in &mut current_defs {
                    rebind_tracked_aggregate_to_chain_set(current);
                }
            }

            // CR 603.2c: An anaphor left on the TRIGGERING-BATCH reading in a
            // chain that has NO trigger event has nothing to refer to — the
            // aggregate would reduce an empty set to a confident 0. Fail
            // honestly instead. (Angrath, Minotaur Pirate's loyalty ability:
            // "Destroy all creatures target opponent controls. Angrath deals
            // damage to that player equal to their total power.")
            if !ir.in_trigger {
                let fragment = clause_ir
                    .where_x_expression
                    .as_deref()
                    .map(|expr| format!("where X is {expr}"))
                    .unwrap_or_else(|| clause_ir.source.fragment().unwrap_or_default().to_string());
                for current in &mut current_defs {
                    demote_unbindable_batch_aggregate(current, &fragment);
                }
            }

            // Find the previous non-special, non-absorbed clause
            let prev_effect = defs.last().map(|d| &*d.effect);
            if let Some(prev_eff) = prev_effect {
                // CR 603.7c: Stamp the prior clause's zone destination as the
                // expected origin of any delayed `ParentTarget` return, so the
                // resolver's CR 400.7 `origin` guard suppresses the return when the
                // snapshotted referent has left that zone. Sibling of (not nested
                // in) the tracked-set rewrite above — this must fire for the
                // non-anaphor "that card" phrasing too.
                //
                // CR 406.1: `ExileTop` always moves cards to `Zone::Exile`. Without
                // this arm the Necropotence / Bomat Courier class's delayed return
                // ("put that card into your hand at the beginning of your next end
                // step") would not have its `origin: Exile` stamped, so the
                // resolver's referent-zone guard would erroneously suppress the
                // recall even when the card is still in exile.
                let prev_zone: Option<Zone> = match prev_eff {
                    Effect::ChangeZone { destination, .. }
                    | Effect::ChangeZoneAll { destination, .. } => Some(*destination),
                    Effect::ExileTop { .. } => Some(Zone::Exile),
                    _ => None,
                };
                if let Some(zone) = prev_zone {
                    for current in &mut current_defs {
                        stamp_delayed_returns(&mut current.effect, zone);
                    }
                }
            }
        }

        defs.extend(current_defs);
        env.observe(&defs, Some(clause_ir.id), NodeRole::Primary);

        // Apply intrinsic continuation after extending defs with current clause's
        // defs (`Emit.intrinsic`).
        if let Some(continuation) = clause_ir.disposition.intrinsic() {
            apply_clause_continuation(&mut defs, continuation.clone(), kind, &env);
            env.observe(&defs, None, NodeRole::ContinuationProduct);
            apply_where_x_to_latest_def(&mut defs, clause_ir.where_x_expression.as_deref());
        }

        // CR 608.2c: Advance the separating boundary for the next normal-path
        // clause. Special/absorbed clauses also advance `prev_boundary` (via the
        // `handled_as_special` branch above) — although they emit no sibling
        // def, they occupy a chunk slot and carry their own trailing boundary,
        // so a following normal clause must stamp `sub_link` from the boundary
        // AFTER the special clause, not the stale one that preceded it.
        prev_boundary = clause_ir.boundary;
        // Classify anything this clause detached. The mirror is asserted at the top of
        // the next iteration, and — for the LAST clause, which has no next iteration —
        // at the Phase-1/Phase-2 boundary below.
        env.arena.settle();
    }

    // The terminal-clause blind spot: the loop-top assert validates each clause's
    // mutations on the NEXT iteration, so drift introduced by a card's FINAL clause
    // would never be asserted at all — a rider on the last clause (Brainstealer
    // Dragon's trailing mana rider is the witness) could desync the arena and no
    // assert would ever run again. Phase 2 does not touch `env`, so this is the last
    // moment the mirror still means anything. Assert it here, and Phase 1 is covered
    // end to end.
    env.arena.assert_mirrors(&defs);

    // ── Phase 2: Post-loop assembly (unchanged) ────────────────────────
    let kind = ir.kind;
    let chain_rounding = ir.chain_rounding;

    // CR 701.20a / CR 701.20e: Demote reveal-Dig back to RevealTop when no DigFromAmong
    // continuation patched it. An unpatched Dig { reveal: true, keep_count: None, filter: Any }
    // is a simple "reveal the top N" with no player selection — it must resolve synchronously
    // (via RevealTop) so that sub_ability chains like RevealedHasCardType evaluate inline.
    for def in &mut defs {
        if let Effect::Dig {
            count,
            keep_count: None,
            filter: TargetFilter::Any,
            reveal: true,
            destination,
            rest_destination,
            player,
            ..
        } = &*def.effect
        {
            if destination == &Some(Zone::Library) && rest_destination == &Some(Zone::Library) {
                continue;
            }
            let count_val = match count {
                QuantityExpr::Fixed { value } => *value as u32,
                _ => 1,
            };
            *def.effect = Effect::RevealTop {
                player: player.clone(),
                count: count_val,
            };
        }
    }

    // CR 701.20a + CR 608.2c: A bare private "look at the top N cards" instruction
    // is only a look; it does not move a chosen card to hand. Continuations that
    // actually choose cards from among them patch destination/keep_count before this
    // pass. Anything still in the raw private-Dig shape is a pure peek: skip
    // DigChoice and only populate last_revealed_ids for downstream conditions.
    for def in &mut defs {
        if let Effect::Dig {
            reveal: false,
            keep_count: None,
            keep_count_expr: None,
            filter: TargetFilter::Any,
            destination: None,
            rest_destination: None,
            ..
        } = &*def.effect
        {
            if let Effect::Dig { keep_count, .. } = &mut *def.effect {
                *keep_count = Some(0);
            }
        }
    }

    // CR 702.33d + CR 608.2c: Resolve "create [N] of those tokens [instead]"
    // anaphoric subs — the sub-ability parses as `Unimplemented` because the
    // noun "those tokens" refers back to the previous clause's token-creation
    // effect. Rewrite those subs by cloning the previous effect with an
    // updated count (Rite of Replication / Saproling Migration / Krothuss).
    resolve_those_tokens_anaphors(&mut defs);
    resolve_populated_unsuspect_anaphors(&mut defs);

    // CR 701.36a + CR 603.7c: Resolve "the token created this way …" and the
    // "sacrifice it" anaphors that follow a token-creating effect (Populate,
    // CopyTokenOf, Token). The antecedent is the populated / created token;
    // `TargetFilter::LastCreated` at runtime resolves against
    // `state.last_created_token_ids` (snapshotted at delayed-trigger
    // creation for the Sacrifice case — CR 603.7c).
    resolve_populated_token_anaphors(&mut defs);

    // CR 707.12: "Copy [a card]. You may cast the copy ..." is not a stack
    // copy (CR 707.10). It creates a card copy in the source zone, then casts
    // that copy during resolution. Fold the two parsed imperative clauses into
    // the dedicated engine primitive before generic chain assembly.
    fold_cast_copy_of_card_defs(&mut defs);

    // CR 706 + CR 705: Consolidate die result table lines into their parent RollDie,
    // and coin flip conditional branches into their parent FlipCoin.
    consolidate_die_and_coin_defs(&mut defs, kind);

    // CR 609.7a + CR 608.2c: Desperate Gambit — a preceding
    // `ChooseDamageSource` makes bare "it" in the lose-branch one-shot prevention
    // refer to the chosen source, not `SelfRef` (the instant on the stack).
    thread_chosen_damage_source_into_oneshot_effects(&mut defs);

    // Chain: last has no sub_ability, each earlier one chains to next.
    // When a def already has a sub_ability (e.g., TargetOnly with attached Explore),
    // append to the deepest sub rather than overwriting.
    let mut result = if defs.len() > 1 {
        let last = defs.pop().unwrap();
        let mut chain = last;
        while let Some(mut prev) = defs.pop() {
            // R1 — a SHAPE REPAIR, not materialization.
            merge_search_tail_into_additional_cost_else(&mut prev, &chain);
            // A node attached as a `sub_ability` is a resolution continuation
            // of its parent, not an independently activatable ability. Ordinary
            // chains normalize it to `Spell`; an IR producer can preserve a
            // legacy enclosing kind when that is part of its lowered shape.
            chain.kind = continuation_kind;
            // R2 — a SHAPE REPAIR, not materialization.
            normalize_linked_exile_cast_pair(&mut prev, &mut chain);
            // CR 608.2c: an independent sentence after an if/otherwise choice
            // resolves after either branch (for example, Wedding Announcement's
            // three-counter transform also follows its Human-token branch).
            if chain.sub_link == SubAbilityLink::SequentialSibling {
                if let Some(else_chain) = prev.else_ability.as_mut() {
                    append_to_deepest_sub_ability(else_chain, Some(Box::new(chain.clone())));
                }
            }
            if prev.sub_ability.is_some() {
                // Walk to the deepest sub_ability and append there
                let mut cursor = &mut prev;
                while cursor.sub_ability.is_some() {
                    cursor = cursor.sub_ability.as_mut().unwrap();
                }
                // R3 — a SHAPE REPAIR, not materialization.
                rebind_condition_instead_damage_anaphor(cursor, &mut chain);
                cursor.sub_ability = Some(Box::new(chain));
            } else {
                prev.sub_ability = Some(Box::new(chain));
            }
            chain = prev;
        }
        chain
    } else {
        defs.pop().unwrap_or_else(|| {
            AbilityDefinition::new(
                kind,
                // `description: None` is load-bearing: `Effect::unimplemented()` would force
                // `Some(..)` and change serialized output, so the conversion is deferred out
                // of this byte-identical move.
                // allow-noncombinator: pre-existing literal relocated verbatim from lower.rs (U6-A byte-identical move); conversion deferred
                Effect::Unimplemented {
                    name: "empty".to_string(),
                    description: None,
                },
            )
        })
    };

    // CR 608.2 + CR 107.2: Ordinary parsed clauses rewrite target-scoped refs
    // ("their life", "their hand") to their per-iterating-player equivalents.
    // Whole-body recognizers can preserve explicitly constructed scoped fields;
    // the walk still covers a scoped clause buried under earlier non-scoped
    // clauses (Betor, Kin to All).
    if matches!(ir.player_scope_rewrite, PlayerScopeRewrite::Apply) {
        apply_player_scope_rewrites(&mut result);
    }

    // CR 107.1a: Apply the chain-level rounding annotation (captured above)
    // to every DivideRounded in the built tree. No-op when the sentence was
    // absent (chain_rounding == None).
    if let Some(mode) = chain_rounding {
        rewrite_rounding_mode(&mut result, mode);
    }

    collapse_ephemeral_color_choice_mana(&mut result);
    // CR 608.2c + CR 615.1a + CR 615.4: Fold "deal N damage … prevent X of that
    // damage" (Power Leak, Errant Minion) into one computed-amount DealDamage
    // (max(N − X, 0)). Must run after where-X binding has populated the
    // prevention node's `amount_dynamic`, which happens during IR lowering above.
    fold_deal_damage_then_prevent_into_computed_amount(&mut result);
    // CR 105.4 + CR 702.16: inject a color choice ahead of a "gains
    // protection/hexproof from the color of your choice" grant so the source
    // carries a chosen color for the layer applier to bake in.
    inject_chosen_color_choice_grant(&mut result, false);
    rewrite_that_type_mana_instead(&mut result);

    fold_token_it_has_grants_into_token_statics(&mut result);
    fold_copy_spell_gains_haste_and_quoted_grant(&mut result);
    nest_whenever_this_turn_token_cleanup_delayed_trigger(&mut result);
    // CR 303.4f + CR 301.5b + CR 603.7d: Wire `forward_result: true` on a
    // parent zone-change to Battlefield when the chained sub-ability is an
    // `Attach` gated by `ZoneChangedThisWay`. Without this, the runtime
    // resolves the sub-ability with `source_id` = the original ability source
    // (the trigger source / Saga / activated permanent), so the Attach tries
    // to equip *that* object to the chosen creature — wrong for Armored
    // Skyhunter (Skyhunter cannot equip itself), wrong for Vault 101: Birthday
    // Party (a Saga is not Equipment), wrong for Quest for the Holy Relic and
    // Stonehewer Giant (the searcher is not the moved Equipment).
    //
    // CR 608.2c: The same flag also wires sub-chains whose own clauses
    // anchor on the just-moved card via the bare-"it" anaphor
    // (`TargetFilter::SelfRef`) — Emperor of Bones' "[…] put a creature
    // card exiled with this creature onto the battlefield […]. It gains
    // haste. Sacrifice it at the beginning of the next end step." The
    // trailing GenericEffect/Pump and CreateDelayedTrigger subs target
    // `SelfRef` so the runtime's `source_id` rewrite resolves them to the
    // moved card instead of Emperor itself.
    //
    // The `forward_result` flag makes the runtime forward the just-moved
    // card's id as the sub-ability's `source_id` (see `effects/mod.rs`
    // forward_result branch), so `Attach::resolve` operates on the correct
    // attaching object.
    rewire_result_anchored_subchain(&mut result);
    fold_enters_this_way_counter_rider(&mut result);
    // CR 603.7a + CR 608.2c + CR 702.170c: fold the exile-instead "If you do,
    // ..." continuation (Feather's return-to-hand, Lilah's become-plotted) onto
    // the exile-resolving carrier's typed `on_exile` rider so the consequence is
    // applied when the replacement is APPLIED (spell lands in exile), not when
    // the trigger resolves.
    fold_exile_resolving_rider(&mut result);
    wire_optional_cast_decline_fallback(&mut result);
    retarget_counter_additional_cost_to_target(&mut result);
    // CR 608.2c: two-target "put a counter on the you-control creature, then those
    // creatures fight each other" (Malamet/Longstalk/Duel) — re-key the counter's
    // ParentTarget anaphor to chain slot 0 and bind its condition subject to the
    // same slot under most-recent-only propagation.
    rewrite_two_target_counter_chain(&mut result);
    // CR 608.2c + CR 608.2b: resolve a chained tap/untap anaphor against a
    // SelfRef-subject head (The Incredible Hulk's "untap him") — rewrite its
    // ParentTarget to SelfRef so it binds the source, while a real/optional
    // target head (Tyvar Kell) keeps ParentTarget and no-ops when declined.
    patch_self_ref_head_tap_anaphor(&mut result);
    // CR 608.2c + CR 122.1: bind a mass `PutCounterAll` head's chained "untap
    // them" to the countered set (Lulu, Loyal Hollyphant).
    patch_population_head_tap_anaphor(&mut result);
    // CR 608.2c: bind a "choose a card …, then {put|remove} counters {on|from} it"
    // continuation's "it" anaphor to the chosen card (Amy Pond). The standalone
    // counter clause lowers "it" to SelfRef; under an `Effect::ChooseFromZone`
    // head it must read the chosen object the `ChooseFromZoneChoice` handler
    // installs as the continuation target, so rewrite SelfRef → ParentTarget.
    patch_choose_from_zone_counter_continuation_target(&mut result);
    // CR 601.2c + CR 608.2c: suppress a reflexive-target rider when the optional
    // "up to one" antecedent target is declined (no object target chosen).
    gate_reflexive_rider_on_declined_optional_target(&mut result);
    // CR 608.2c + CR 613.1f: persist a standalone "choose a [type] card exiled
    // with ~" pick as the host's last chosen card (Koh, the Face Stealer).
    append_remember_card_to_standalone_exiled_choice(&mut result);
    fold_search_choose_type_conditional_destination(&mut result);
    if matches!(&*result.effect, Effect::SearchOutsideGame { .. }) {
        result.optional = false;
        result.optional_for = None;
    }

    // CR 608.2c PRECONDITION (not a card-name suppression): "the card revealed by
    // the OTHER player" DENOTES a card only when the same assembled chain carries a
    // multi-player reveal fan-out. `ObjectScope::OtherRevealedCard` is well-formed
    // only when a sibling `RevealTop` distributes to >=2 players (`multi_target`
    // present). Applied on the FINAL tree (this chokepoint sees the complete
    // structure regardless of when `multi_target` was attached), so the presence
    // check is robustly correct.
    gate_other_revealed_card_on_multiplayer_reveal(&mut result);

    // CR 608.2c + CR 107.1c: A trailing "repeat this process" directive sets a
    // chain-level loop predicate; apply it to the assembled root ability so the
    // resolver re-follows the whole chain.
    if let Some(ref continuation) = ir.repeat_until {
        result.repeat_until = Some(continuation.clone());
    }

    // CR 607.2d: fill every committed-choice guess with the head Choose's domain.
    super::propagate_committed_choice_type_to_guesses(&mut result);
    // CR 608.2d: gate the whole "if they guessed wrong/right" branch, including
    // any "and ..." continuation steps.
    super::propagate_guess_branch_condition_to_continuations(&mut result);

    result
}

// ===========================================================================
// The three SHAPE REPAIRS hidden in the final fold (U6-C4)
// ===========================================================================
//
// The fold does two jobs: pure materialization (link each def into the next via
// `sub_ability`), and these three adjacency-sniffing semantic repairs. They were
// anonymous, interleaved with the linking, and each fires on a handful of cards
// out of 35,396 — so a materialization rewrite could drop all three and the suite
// would stay green. Naming them means C5's diff has to VISIBLY keep calling them.
//
// C4 is pure code motion: bodies are byte-identical, call order is unchanged.

/// R1 — CR 608.2c + CR 601.2b: the ELSE-SIDE half of `FoldSearchIntoElse`.
///
/// One rules concept implemented in two places. `ClauseDisposition::FoldSearchIntoElse`
/// binds the *search* side at clause time (by provenance, since U6-C2); this fold-time
/// repair binds the *else* side by SHAPE-SNIFFING the pair. When an additional cost was
/// paid (CR 601.2b) and both the stashed base chain and the incoming tail are
/// search-destination moves, the tail's continuation is appended to the base's deepest
/// sub — later text modifying the meaning of earlier text (CR 608.2c).
///
/// Note it reads `chain.sub_ability` only; `chain` itself is linked normally afterward.
/// Returns whether the repair fired.
fn merge_search_tail_into_additional_cost_else(
    prev: &mut AbilityDefinition,
    chain: &AbilityDefinition,
) -> bool {
    if prev.condition == Some(AbilityCondition::AdditionalCostPaidInstead) {
        if let Some(base_chain) = prev.else_ability.as_mut() {
            if matches!(
                (&*base_chain.effect, &*chain.effect),
                (
                    Effect::ChangeZone {
                        origin: Some(Zone::Library),
                        destination: Zone::Hand,
                        ..
                    },
                    Effect::ChangeZone {
                        origin: Some(Zone::Library),
                        destination: Zone::Hand,
                        ..
                    }
                )
            ) {
                append_to_deepest_sub_ability(base_chain, chain.sub_ability.clone());
                return true;
            }
        }
    }
    false
}

/// R2 — CR 608.2c + CR 401.4: linked-exile-cast bottom cleanup.
///
/// After an optional `CastFromZone` from a linked exile, the trailing "put it on the
/// bottom" cleanup is normalized in place AND stashed as `prev.else_ability`.
///
/// DELIBERATE, DO NOT "FIX": `chain` is cloned into `else_ability` here and is ALSO
/// linked as `prev`'s sub by the caller below, so the node is reachable via two paths.
/// That duplication is the existing behavior; preserving it is the point of C4.
/// Returns whether the repair fired.
fn normalize_linked_exile_cast_pair(
    prev: &mut AbilityDefinition,
    chain: &mut AbilityDefinition,
) -> bool {
    if prev.optional && is_linked_exile_cast_bottom_cleanup(&prev.effect, &chain.effect) {
        normalize_linked_exile_cast_bottom_cleanup(&mut chain.effect);
        prev.else_ability = Some(Box::new(chain.clone()));
        return true;
    }
    // CR 608.2c + CR 701.13a: Jodah, the Unifier — the head-aware companion
    // gate. `prev` is `ExileFromTopUntil { NextMatches }`; `chain` is its
    // optional `CastFromZone { ParentTarget }` with the bottom cleanup already
    // nested beneath it. Rewrite that cleanup to the linked exile set and make
    // it the decline branch, preserving the hit in exile when the cast is
    // declined.
    if chain.optional {
        if let Some(cleanup) = chain.sub_ability.as_deref().cloned() {
            if is_exile_until_cast_bottom_cleanup(&prev.effect, &chain.effect, &cleanup.effect) {
                if let Some(cleanup_mut) = chain.sub_ability.as_deref_mut() {
                    normalize_exile_until_cast_bottom_cleanup(&mut cleanup_mut.effect);
                    chain.else_ability = Some(Box::new(cleanup_mut.clone()));
                    return true;
                }
            }
        }
    }
    false
}

/// R3 — CR 120.1 + CR 208.1 + CR 608.2c: a "Then it deals damage equal to
/// its power to <fresh opponent>" tail appended after a `ConditionInstead`
/// override is the same one-sided-fight anaphor as the non-nested Ambuscade
/// form ("It" = the boosted creature = Target1, the source; "its power" read
/// live). The generic fold loop appends it without the chunk-loop's anaphor
/// rebind, so it would otherwise keep the subject-stamping default
/// (`Power{Source}` + `damage_source: None` → 0 damage from the spell). Reuse
/// the one-sided-fight rebind to restore `Power{Anaphoric}` + `DamageSource::
/// Target`. No-op (returns false, mutates nothing) for non-damage /
/// non-fresh-opponent tails (Evil's Thrall's Untap, the Draw tails). Gated to
/// the override cursor + an independent `SequentialSibling` tail so non-nested
/// Ambuscade/Bite Down/Rabid Bite (rebound at the chunk-loop site) are
/// untouched.
///
/// Returns whether the rebind actually mutated the tail (the gate can pass while the
/// rebind is a no-op — see above), so a fire count measures real repairs, not gate hits.
fn rebind_condition_instead_damage_anaphor(
    cursor: &AbilityDefinition,
    chain: &mut AbilityDefinition,
) -> bool {
    if matches!(
        cursor.condition,
        Some(AbilityCondition::ConditionInstead { .. })
    ) && chain.sub_link == SubAbilityLink::SequentialSibling
    {
        return bind_anaphoric_damage_subject_keep_recipient(chain.effect.as_mut());
    }
    false
}

#[cfg(test)]
mod arena_tests {
    use super::*;

    fn shuffle_def() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
        )
    }

    /// The mirror assert's IDENTITY check (`order[i]` names the def actually at
    /// `defs[i]`) guards the mid-vector-removal defect — and that defect is LATENT on
    /// today's card pool: both `FoldSearchIntoElse` removals happen to land on the
    /// tail, where a tail pop coincidentally mirrors them correctly. So nothing in the
    /// suite or the full-pool sweep has ever watched this assert go red, and a guard
    /// nobody has seen fail is a guard nobody should trust.
    ///
    /// This drives the defect synthetically: remove `defs[1]`, then mirror it the way
    /// the pre-fix code did — with `sync_len`'s TAIL pop. Note what stays perfectly
    /// consistent: 3 defs and 3 nodes become 2 and 2. Every count lines up. The old
    /// count-only assert accepted exactly this. Only IDENTITY diverges — `order[1]`
    /// still names the def that was removed, not the one that shifted into its place.
    #[test]
    #[should_panic(expected = "order` names a different def than `defs` holds")]
    fn tail_pop_mirroring_a_mid_vector_removal_is_caught() {
        let mut defs = vec![shuffle_def(), shuffle_def(), shuffle_def()];
        let mut arena = Arena::default();
        arena.sync_len(&defs, None, NodeRole::Primary);
        arena.assert_mirrors(&defs);

        defs.remove(1); // MID-vector removal ...
        arena.sync_len(&defs, None, NodeRole::Unknown); // ... mirrored by a TAIL pop.

        // The handler still RESOLVES its detach, exactly as every real one does — the
        // pre-fix bug was never a dangling node, it was WHICH node got detached. So
        // absorb the (wrong) one, which is what the old `settle` did by inferring a
        // parent. `settle` then has nothing to object to, and the identity assert is
        // left to catch the divergence on its own.
        //
        // The parent is `id_at(1)`, NOT `id_at(0)`, so this test does not depend on the
        // ORDER of the asserts. The stray node is the tail one, whose def is the one
        // that shifted into `defs[1]`; naming `id_at(1)` as its parent therefore makes
        // the absorbed-parenthood assert (b) genuinely TRUE (`live_root_index` lands on
        // `defs[1]`, which IS that def), leaving assert (a) — identity — as the only
        // false one. Absorbing into `id_at(0)` would ALSO trip (b), and (a) would only
        // be the observed failure because it happens to run first.
        let stray = arena.detached[0];
        let parent = arena.id_at(1).expect("defs[1] is live");
        arena.absorb(stray, parent);
        arena.settle();

        arena.assert_mirrors(&defs);
    }

    /// `settle`'s invariant, and the reason `NodeStatus::Dropped` is gone.
    ///
    /// A handler that removes a def from `defs` must say where it went — `absorb` when
    /// it nests the def, `reinstate` when it returns it to top-level. U6-0c recorded a
    /// handler that said NOTHING as `Dropped` ("the def left the output"). But the
    /// parenthood assert SKIPS `Dropped` nodes, precisely because a def that left the
    /// output has nothing left to check against — so "I forgot to name the parent" was
    /// accepted as "there is no parent", silently, with the arena holding the wrong
    /// model and no assert objecting.
    ///
    /// That is the same silent-lie class U6-0 exists to remove, reintroduced by the fix
    /// for it. This is the shape it was swallowing, and it now fails.
    #[test]
    #[should_panic(expected = "never absorbed or reinstated")]
    fn a_detached_node_the_handler_never_resolved_is_caught() {
        let mut defs = vec![shuffle_def(), shuffle_def()];
        let mut arena = Arena::default();
        arena.sync_len(&defs, None, NodeRole::Primary);

        // A handler pops a def and nests it under the survivor — but forgets to absorb.
        let popped = defs.pop().expect("two defs");
        defs[0].sub_ability = Some(Box::new(popped));
        arena.sync_len(&defs, None, NodeRole::Unknown); // detaches the node ...
        arena.settle(); // ... and never said where it went.
    }

    /// The TERMINAL-CLAUSE blind spot, and why the Phase-1/Phase-2 boundary assert
    /// exists. The loop-top assert validates a clause's mutations on the NEXT
    /// iteration — so a card's FINAL clause has no iteration after it, and drift it
    /// introduces was asserted by nothing at all. Brainstealer Dragon's trailing mana
    /// rider is the real witness: it corrupted the arena and only an output diff
    /// caught it.
    ///
    /// This is the shape the boundary assert catches — a rider that mutated `defs` and
    /// never told the arena. Note it is also the shape `sync_len` SELF-HEALS if it is
    /// allowed to run first: it would push a fresh node carrying the wrong provenance
    /// and hand it to role membership, which is why the drift must be caught, not
    /// reconciled.
    #[test]
    #[should_panic(expected = "live node count != defs len")]
    fn a_terminal_clause_that_mutates_defs_without_telling_the_arena_is_caught() {
        let mut defs = vec![shuffle_def(), shuffle_def(), shuffle_def()];
        let mut arena = Arena::default();
        arena.sync_len(&defs, None, NodeRole::Primary);

        defs.pop(); // a terminal rider mutates `defs` ...
        arena.assert_mirrors(&defs); // ... and never told the arena.
    }

    /// The green half of the pair. Without it, the `should_panic` above proves only
    /// that the assert CAN fail — not that it DISCRIMINATES. `remove_at` models the
    /// same removal correctly, and the same assert accepts it.
    #[test]
    fn remove_at_mirroring_a_mid_vector_removal_is_accepted() {
        let mut defs = vec![shuffle_def(), shuffle_def(), shuffle_def()];
        let mut arena = Arena::default();
        arena.sync_len(&defs, None, NodeRole::Primary);

        let absorbed = arena.remove_at(1); // the removal, MODELLED ...
        let removed = defs.remove(1); // ... rather than approximated.
        arena.sync_len(&defs, None, NodeRole::Unknown);

        // Nest the removed def under the survivor, exactly as a folding handler does,
        // and name that parent — then the parenthood assert has something true to find.
        defs[0].else_ability = Some(Box::new(removed));
        let parent = arena.id_at(0).expect("defs[0] is live");
        arena.absorb(absorbed, parent);

        arena.settle();
        arena.assert_mirrors(&defs);
        assert!(matches!(
            arena.node(absorbed).status,
            NodeStatus::Absorbed { into } if into == parent
        ));
    }
}

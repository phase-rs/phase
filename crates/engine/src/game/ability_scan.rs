//! CR 603.3b + CR 603.4 + CR 106.1 / CR 119 / CR 122.1: the PR-6.25/PR-6.5 fail-closed AST
//! scanner — a single compiler-exhaustive, wildcard-free walk of a resolved
//! ability's typed AST that answers three independent classification questions
//! ("axes") used by trigger ordering (CR 603.3b) and the growing-cascade
//! coverability detector (`analysis::resource`):
//!
//! 1. **event-context read** — does the ability read a characteristic of the
//!    concrete triggering event / cost-paid object (CR 603.4 / CR 608.2k)? Two
//!    order-independent-looking triggers off *distinct* events are only truly
//!    interchangeable if neither reads the event that distinguishes them.
//! 2. **sibling-mutable read** — does the ability read a source/recipient or
//!    board-scoped mutable P/T / counter aggregate that a sibling copy resolving
//!    first could change (the Rubblebelt Rioters / Orcish Siegemaster class)?
//! 3. **projected-resource read** — does the ability read a player-level monotone
//!    resource or per-turn/per-game journal that
//!    `analysis::resource::project_out_resources` zeroes/clears (life CR 119,
//!    floating mana CR 106.1, poison/energy/player counters CR 122.1, and the
//!    per-turn tally/journal block)? Object counters and marked damage are NOT on
//!    this axis — they are strict-compared by gate (1) of
//!    `loop_states_cover_modulo_growth` (R5-B1), so an object-counter reader
//!    (`CountersOn`/`Power`/`Toughness`) classifies as a NON-reader here.
//!
//! # Why hand-rolled and wildcard-free
//!
//! The soundness of both consumers rests on the scanner being **fail-closed on
//! future variants**: a new `Effect`/`QuantityRef`/`TriggerCondition`/… variant
//! must fail to compile until it is given an explicit reads/doesn't-read decision
//! on every axis. A `_ =>` wildcard (or a serde-tag string walk) silently defeats
//! that — a new event-context or resource reader would be classified inert and
//! ride a false auto-resolution / false coverability win. Therefore every arm is
//! explicit; provably-inert variants get a one-line `Axes::NONE` arm. Types the
//! walk does not descend into (`ContinuousModification`, `ManaProduction`,
//! `ReplacementDefinition`, a nested `ResolvedAbility`, `FilterProp`, the
//! per-mode `AbilityDefinition`s of a reflexive-modal trigger (`mode_abilities`),
//! …) that can transitively express a read are classified **conservatively**
//! (`Axes::CONSERVATIVE` — reads on every axis), the fail-safe direction for all
//! three consumers (over-prompt / over-reject, never a false auto-resolve or
//! false win). `RestrictionPlayerScope` and `CastManaObjectScope` are also in the
//! conservative set: their only carriers (`Effect::AddRestriction` /
//! `AddTargetReplacement`, `QuantityRef::ManaSpentToCast`) already return
//! `Axes::CONSERVATIVE`, so the scopes themselves are never traversed.
//!
//! # Traversal closure (R4-G2)
//!
//! The compiler-exhaustiveness floor holds only for TRAVERSED subtrees: an
//! untraversed payload is silently skipped with no compile error, so the traversal
//! set is part of the trusted base. It is closed under payload reachability across
//! `Effect`, `QuantityRef`, `QuantityExpr`, `AbilityCondition`, `TargetFilter`,
//! `ObjectScope`, `TriggerCondition`, `Duration` (its `ForAsLongAs` `StaticCondition`),
//! `StaticCondition`, `PlayerFilter`, `ReplacementCondition`, the target-count and
//! target-set specs (`MultiTargetSpec`, `TargetSelectionConstraint`), the loop and
//! modal headers (`RepeatContinuation`, `ModalChoice`), and the player scope
//! selectors (`PlayerScope`, `ControllerRef`, `CountScope`). The `ResolvedAbility`
//! and `ModalChoice` fields are destructured without `..`, so a new field must be
//! classified before it compiles. Any type outside this set that can reach a read
//! is in the conservative set above.
//!
//! # Resolution-time choice classifier (a SEPARATE question family)
//!
//! Alongside the three read-axes lives an independent classifier
//! (`effect_resolution_choice_freedom` / `ability_resolution_choice_freedom`,
//! consumed by `analysis::resource::loop_states_cover_modulo_growth` item 6)
//! answering a FOURTH, orthogonal question (CR 608.2d): can resolving this
//! ability enter a resolution-time player choice (a non-priority `WaitingFor`)?
//! This is deliberately NOT a fourth `Axes` axis — `Axes::NONE` means "no
//! reads", which is orthogonal to "never prompts" (`Effect::Scry` reads nothing
//! yet always prompts), so folding a choice bit into `Axes` would make every
//! existing `NONE` arm silently claim choice-freeness. The classifier is
//! fail-closed (`MayPrompt` default — an unproven claim only costs a
//! false-negative cover rejection); promoting a variant to a choice-free
//! verdict is a SOUNDNESS claim ("resolving can never enter a non-priority
//! `WaitingFor`, for ANY state") and requires a resolver trace cited in the arm
//! plus a `..`-free destructure so a future field forces re-audit.
//!
//! # Consumers of the read-axis classifiers after PR-6.75
//!
//! CR 603.3b: the legacy UNGATED trigger-ordering paths (same firing event, and
//! the explicitly-simultaneous ZoneChanged departure batch) no longer consume the
//! event-context / sibling-mutable read classifiers of this scanner. They consume
//! the richer kind/scope read/write conflict profile in the sibling module
//! `ability_rw.rs` (`ability_rw_profile` / `trigger_condition_rw_profile` /
//! `profiles_conflict`), which answers "which kinds of state does the ability READ
//! and WRITE, at what scope" — the precise read/write predicate those paths require
//! (PR-6.25 §3 C0(ii)). The event-context and sibling-mutable read classifiers here
//! are now consumed ONLY by the C2 distinct-event term (`group_is_order_independent`
//! / `trigger_events_match_for_ordering`), ungated from loop detection (adopted from
//! #5084) and conjoined with `!batch_conflict` — so a coarse C2-clean verdict may
//! auto-order a distinct-event group only when the precise `ability_rw` profiler also
//! agrees it is conflict-clean; a conservative verdict here means a prompt (safe
//! over-reject). The projected-resource classifier (question 3) and the
//! resolution-time choice classifier (question 4) are unchanged. See `ability_rw.rs`
//! for the conflict model and its CR 603.3b commutation argument.

use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ContinuousModification, ControllerRef,
    CountScope, Duration, EachDamageRecipient, Effect, EffectScope, FilterProp,
    ForEachCategoryAction, GuessSubject, KeeperConstraint, ManaProduction, ModalChoice,
    MultiTargetSpec, ObjectScope, PlayerFilter, PlayerScope, PtValue, QuantityExpr, QuantityRef,
    RepeatContinuation, ReplacementCondition, ResolvedAbility, StaticCondition, TargetChoiceTiming,
    TargetFilter, TrackedAnaphorSource, TriggerCondition, TypedFilter,
};
use crate::types::game_state::TargetSelectionConstraint;
use crate::types::keywords::{DisguiseCost, Keyword};

/// The three independent classification axes, accumulated over one AST walk.
/// `true` on an axis means "reads (or may read) that dimension"; the fail-safe
/// direction for every consumer.
#[derive(Clone, Copy)]
struct Axes {
    /// Reads a concrete-triggering-event / cost-paid-object characteristic
    /// (CR 603.4 / CR 608.2k). Used by trigger ordering to keep distinct-event
    /// groups from auto-resolving.
    event: bool,
    /// Reads a source/recipient or board-scoped mutable aggregate a sibling copy
    /// could mutate (CR 603.3b ordering-relevance).
    sibling: bool,
    /// Reads a player-level monotone resource / per-turn journal that
    /// `project_out_resources` neutralizes (CR 106.1 / CR 119 / CR 122.1).
    projected: bool,
}

impl Axes {
    /// No read on any axis.
    const NONE: Axes = Axes {
        event: false,
        sibling: false,
        projected: false,
    };
    /// A subtree the walk does not descend into but which can transitively express
    /// a read — classified as reading everything (fail-closed / fail-safe).
    const CONSERVATIVE: Axes = Axes {
        event: true,
        sibling: true,
        projected: true,
    };

    fn or(self, other: Axes) -> Axes {
        Axes {
            event: self.event || other.event,
            sibling: self.sibling || other.sibling,
            projected: self.projected || other.projected,
        }
    }
}

/// Which consumer is asking, and thus how the two mode-divergent arms
/// (`Effect::Token`, `Effect::Mana`) classify.
///
/// `Conservative` is the pre-existing shared answer that the CR 603.3b
/// trigger-ordering gate (`game::triggers`) and every non-firewall caller
/// require, and it keeps the `LoopDetectionMode::Off` game byte-identical (#4603).
/// `LoopFirewall` is used ONLY by the CR 732.2a object-growth firewall
/// (`analysis::resource`), which needs the two token/mana blankets to DESCEND
/// rather than fail closed. Every other arm is mode-invariant, so `Off` cannot
/// observe `LoopFirewall` — the divergent arms are reachable only through the
/// firewall's `*_for_loop` entry points, themselves reachable only under
/// `loop_detection.samples()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanMode {
    /// Fail-closed on `Token`/`Mana` (the shared CR 603.3b + default answer).
    Conservative,
    /// Descend `Token`/`Mana` bodies (CR 732.2a firewall only).
    LoopFirewall,
}

/// How a given `scan_target_filter` CALL SITE reads its filter — the census
/// discipline the CR 732.2a object-growth firewall's `Typed` relaxation depends
/// on. This is analysis plumbing (a sibling of [`ScanMode`]), NOT a game-semantic
/// variant: it records whether the caller is counting/testing LIVE battlefield
/// membership (a board census whose `sibling` read is the census's OWN — never
/// inherited from the filter, never relaxed) or is naming a snapshot / triggering
/// event / single-object target (where `sibling` may only come from a genuine
/// board-reading component of the filter).
///
/// It is a REQUIRED parameter of [`scan_target_filter`] with NO `Default` impl, so
/// no caller — present or future — can obtain a filter's axes without stating its
/// census intent; the old fail-open delegation path (inheriting the relaxed `Typed`
/// verdict) no longer compiles (REQ-1 structural closure).
///
/// **ADD-1 default-to-census:** an AMBIGUOUS or newly-added call site is
/// [`FilterReadContext::LiveBoardCensus`]. Given the firewall direction
/// (`LiveBoardCensus` ⇒ `sibling:true` ⇒ VETO; `SnapshotOrEvent` ⇒ relaxed ⇒ may
/// OFFER), a misjudgment toward census can only OVER-veto (miss a legal offer),
/// never produce a false offer (CR 732.2a: a coarse relation may reject, never
/// accept; #4603-preserving). Only a POSITIVELY-PROVEN snapshot/event site earns
/// `SnapshotOrEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterReadContext {
    /// This call site counts or tests LIVE battlefield membership. The `sibling`
    /// axis is the census's own read, injected by the wrapper `base` independent of
    /// the filter's shape, and is NEVER relaxed under `LoopFirewall`.
    LiveBoardCensus,
    /// The filter names a target, triggering event, or cast-time snapshot. `sibling`
    /// arises only from a genuine board-reading component of the filter, so a bare
    /// `Typed` under `LoopFirewall` is relaxed (the CR 732.2a coverability gate).
    SnapshotOrEvent,
}

/// Walk a resolved ability's read-bearing fields.
///
/// The `ResolvedAbility` destructure below is **exhaustive with no `..` rest
/// pattern** — the struct-level analogue of the walk's no-wildcard match
/// discipline. Every field is either scanned (read-bearing) or bound to `_`
/// with a one-line "read-free" justification; a FUTURE field added to
/// `ResolvedAbility` fails to compile here until it is classified, closing the
/// "unread aux field" hole class at compile time (not just `multi_target` /
/// `target_constraints`).
fn resolved_ability_axes(a: &ResolvedAbility, mode: ScanMode) -> Axes {
    let ResolvedAbility {
        // ---- read-bearing: scanned into `acc` below ----
        effect,
        sub_ability,
        else_ability,
        condition,
        duration,
        player_scope,
        starting_with,
        repeat_for,
        announced_x,
        multi_target,
        target_constraints,
        unless_pay,
        target_chooser,
        repeat_until,
        modal,
        mode_abilities,
        // ---- read-free: concrete ids / cast-time snapshots / flags / links,
        //      none of which express a resolution-time dynamic read ----
        targets: _,                // concrete announced target refs (already resolved)
        source_id: _,              // object id
        source_incarnation: _,     // self-transform epoch latch, no dynamic read
        trigger_source: _,         // exact triggered-source authority, no dynamic read
        trigger_definition_ref: _, // exact trigger occurrence, no dynamic read
        controller: _,             // player id
        original_controller: _,    // player id
        scoped_player: _,          // player id (iteration binding)
        kind: _,                   // AbilityKind tag (no payload)
        context: _,                // SpellContext: cast-time fact snapshot, not a live read
        optional_targeting: _,     // bool
        optional: _,               // bool
        optional_for: _,           // OpponentMayScope: AnyOpponent/AnyPlayer, no read
        target_choice_timing: _,   // Stack/Resolution tag
        description: _,            // display string
        selected_mode_labels: _,   // display strings, no dynamic read
        min_x_value: _,            // u32
        cant_be_copied: _,         // bool
        copy_count_status: _,      // status tag
        forward_result: _,         // bool
        distribution: _,           // concrete pre-assigned (TargetRef, u32) portions
        chosen_x: _,               // concrete cast-time X
        cost_paid_object: _,       // concrete captured-object snapshot
        cost_paid_object_ids: _,   // concrete captured-object ids (issue #4948)
        effect_context_object: _,  // concrete captured-object snapshot
        amassed_army_object: _,    // concrete captured-object snapshot
        ability_index: _,          // usize provenance
        may_trigger_origin: _,     // provenance tag
        target_selection_mode: _,  // Chosen/Random tag
        chosen_players: _,         // concrete chosen player ids
        replacement_applied: _,    // replacement provenance set, no dynamic read
        sub_link: _,               // SubAbilityLink kind tag
        parent_target_missing_reason: _, // seam flag
    } = a;

    let mut acc = scan_effect(effect, mode);
    if let Some(sub) = sub_ability {
        acc = acc.or(resolved_ability_axes(sub, mode));
    }
    if let Some(else_branch) = else_ability {
        acc = acc.or(resolved_ability_axes(else_branch, mode));
    }
    if let Some(condition) = condition {
        acc = acc.or(scan_ability_condition(condition, mode));
    }
    if let Some(duration) = duration {
        acc = acc.or(scan_duration(duration, mode));
    }
    if let Some(player_scope) = player_scope {
        acc = acc.or(scan_player_filter(player_scope, mode));
    }
    if let Some(starting_with) = starting_with {
        acc = acc.or(scan_controller_ref(starting_with));
    }
    if let Some(repeat_for) = repeat_for {
        acc = acc.or(scan_quantity_expr(repeat_for, mode));
    }
    // CR 601.2b: the announce-time-locked definition of X ("where X is <count> as
    // you cast this spell") is a live board read like any other quantity — it is
    // merely READ EARLIER (at announcement) than a resolution-time slot. It is
    // read-bearing and must be scanned, not classified as a cast-time snapshot;
    // `chosen_x` (below) is the concrete VALUE this expression produces.
    if let Some(announced_x) = announced_x {
        acc = acc.or(scan_quantity_expr(announced_x, mode));
    }
    // CR 601.2c / CR 115.1d: variable-count targeting bounds (min/max) are
    // `QuantityExpr`s that can read a projected/event resource (e.g. a die-result X).
    // MultiTargetSpec is itself destructured without `..` (same no-wildcard floor).
    if let Some(MultiTargetSpec { min, max }) = multi_target {
        acc = acc.or(scan_quantity_expr(min, mode));
        if let Some(max) = max {
            acc = acc.or(scan_quantity_expr(max, mode));
        }
    }
    // CR 115.1 / CR 601.2c: cross-target legality constraints; `TotalManaValue`'s
    // where-X bound carries an `EventContextAmount` (axis-1) read.
    for c in target_constraints {
        acc = acc.or(scan_target_selection_constraint(c, mode));
    }
    // CR 605.3a / CR 608.2g: a resolution-time "unless a player pays {cost}"
    // consults floating mana (CR 106.1), a projected axis.
    if unless_pay.is_some() {
        acc.projected = true;
    }
    // CR 601.2c / CR 603.3d: `target_chooser` selects who announces targets; a
    // TargetFilter like `TriggeringSourceController` reads the triggering event.
    if let Some(chooser) = target_chooser {
        acc = acc.or(scan_target_filter(
            chooser,
            FilterReadContext::SnapshotOrEvent,
            mode,
        ));
    }
    // CR 608.2c / CR 107.1c: a "repeat this process while <condition>" predicate is
    // re-evaluated against freshly-resolved state each iteration — a resolution read.
    if let Some(repeat_until) = repeat_until {
        acc = acc.or(scan_repeat_continuation(repeat_until, mode));
    }
    // CR 700.2: a modal header's dynamic mode cap / chooser can read dynamic state.
    if let Some(modal) = modal {
        acc = acc.or(scan_modal_choice(modal, mode));
    }
    // CR 700.2b: reflexive-modal per-mode `AbilityDefinition`s are def-level structs
    // the walk does not descend into — conservative (fail-closed) when present.
    if !mode_abilities.is_empty() {
        acc = acc.or(Axes::CONSERVATIVE);
    }
    acc
}

/// CR 608.2c / CR 107.1c: a loop-continuation predicate. Only `WhileCondition`
/// re-reads game state (per-iteration re-evaluation); the controller-prompted and
/// boolean-stop variants read no dynamic resource.
fn scan_repeat_continuation(r: &RepeatContinuation, mode: ScanMode) -> Axes {
    match r {
        RepeatContinuation::ControllerChoice => Axes::NONE,
        RepeatContinuation::UntilStopConditions {
            stop_on_put_to_hand: _,
            stop_on_duplicate_exiled_names: _,
        } => Axes::NONE,
        RepeatContinuation::WhileCondition {
            condition,
            max_iterations: _,
        } => scan_ability_condition(condition, mode),
    }
}

/// CR 700.2: the read-bearing payloads of a modal header. `dynamic_max_choices`
/// (a `QuantityExpr`) and `chooser` (a `PlayerFilter`) can read dynamic state; the
/// remaining fields are cast/announce-time metadata (concrete counts, costs, and
/// static cast-time predicates) that do not express a resolution-time dynamic read.
/// Destructured without `..` — a future `ModalChoice` field must be classified here.
fn scan_modal_choice(m: &ModalChoice, mode: ScanMode) -> Axes {
    let ModalChoice {
        dynamic_max_choices,
        chooser,
        min_choices: _,
        max_choices: _,
        mode_count: _,
        mode_descriptions: _,
        allow_repeat_modes: _,
        constraints: _, // cast-time modal-cap predicates (announcement-time, not resolution)
        mode_costs: _,
        mode_pawprints: _,
        entwine_cost: _,
        selection: _,
    } = m;
    let mut acc = scan_player_filter(chooser, mode);
    if let Some(qty) = dynamic_max_choices {
        acc = acc.or(scan_quantity_expr(qty, mode));
    }
    acc
}

/// CR 115.1 / CR 601.2c: cross-target legality constraints. Only `TotalManaValue`
/// carries a read — its `value` is a `QuantityExpr` documented to hold the where-X
/// `EventContextAmount` (axis 1); the `Different*` variants are pure structural
/// predicates over the chosen set with no dynamic read.
fn scan_target_selection_constraint(c: &TargetSelectionConstraint, mode: ScanMode) -> Axes {
    match c {
        TargetSelectionConstraint::DifferentTargetPlayers => Axes::NONE,
        TargetSelectionConstraint::DifferentObjectControllers => Axes::NONE,
        TargetSelectionConstraint::SameZoneOwner { zone: _ } => Axes::NONE,
        TargetSelectionConstraint::TotalManaValue {
            value,
            comparator: _,
        } => scan_quantity_expr(value, mode),
    }
}

fn scan_effect(x: &Effect, mode: ScanMode) -> Axes {
    // BLOCKER-1: the census discipline for THIS effect's target reads, derived ONCE
    // (depends only on the effect variant + mode). Passed to every effect-TARGET
    // `scan_target_filter` call below. The mode-divergent `Token`/`Mana` arms pass
    // `SnapshotOrEvent` for their structural owner/attach/recipient selectors (single-
    // player/object references, not board censuses) so a vanilla token stays read-free.
    let target_ctx = effect_target_ctx(x, mode);
    match x {
        Effect::StartYourEngines { player_scope } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_filter(player_scope, mode));
            acc
        }
        Effect::ChangeSpeed {
            player_scope,
            amount,
            direction: _,
            floor: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_filter(player_scope, mode));
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc
        }
        Effect::DealDamage {
            amount,
            target,
            damage_source: _,
            excess: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ApplyPostReplacementDamage {
            context: _,
            target: _,
            amount: _,
            is_combat: _,
        } => Axes::NONE,
        Effect::EachDealsDamageEqualToPower {
            sources,
            recipient,
            extra_source,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(sources, target_ctx, mode));
            acc = acc.or(scan_target_filter(recipient, target_ctx, mode));
            if let Some(extra) = extra_source {
                acc = acc.or(scan_target_filter(extra, target_ctx, mode));
            }
            acc
        }
        Effect::OpponentGuess { guesser, subject } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(guesser));
            acc = acc.or(scan_guess_subject(subject, mode));
            acc
        }
        Effect::SwapChosenLabels {
            first: _,
            second: _,
        } => Axes::CONSERVATIVE,
        Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(sources, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(amount, mode));
            if let EachDamageRecipient::Shared(filter) = recipient {
                acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            }
            acc
        }
        Effect::Draw { count, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Pump { .. } => Axes::CONSERVATIVE,
        Effect::PairWith { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Destroy {
            target,
            cant_regenerate: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Regenerate { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::RemoveAllDamage { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Counter { .. } => Axes::CONSERVATIVE,
        Effect::CounterAll { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        // CR 732.2a: a token-making effect fails closed for the CR 603.3b gate
        // (`Conservative`), but the object-growth firewall (`LoopFirewall`) must
        // DESCEND — a token that reads nothing sibling/projected does not veto an
        // otherwise-bounded loop. Exhaustive 14-field destructure, NO `..`: a new
        // field fails to compile until classified.
        Effect::Token {
            power,
            toughness,
            keywords,
            count,
            owner,
            attach_to,
            static_abilities,
            enter_with_counters,
            // read-free: literal name/types/colors/supertypes and enter-state flags
            // express no resolution-time dynamic read.
            name: _,
            types: _,
            colors: _,
            tapped: _,
            enters_attacking: _,
            supertypes: _,
        } => match mode {
            ScanMode::Conservative => Axes::CONSERVATIVE,
            ScanMode::LoopFirewall => {
                let mut acc = Axes::NONE;
                acc = acc.or(scan_pt_value(power, mode));
                acc = acc.or(scan_pt_value(toughness, mode));
                for kw in keywords {
                    acc = acc.or(scan_keyword(kw, mode));
                }
                acc = acc.or(scan_quantity_expr(count, mode));
                acc = acc.or(scan_target_filter(
                    owner,
                    FilterReadContext::SnapshotOrEvent,
                    mode,
                ));
                if let Some(at) = attach_to {
                    acc = acc.or(scan_target_filter(
                        at,
                        FilterReadContext::SnapshotOrEvent,
                        mode,
                    ));
                }
                // A granted static's condition + its layered modifications (P2-a).
                for sd in static_abilities {
                    if let Some(cond) = &sd.condition {
                        acc = acc.or(scan_static_condition(cond, mode));
                    }
                    for m in &sd.modifications {
                        acc = acc.or(scan_continuous_modification(m, mode));
                    }
                }
                for (_counter_type, qty) in enter_with_counters {
                    acc = acc.or(scan_quantity_expr(qty, mode));
                }
                acc
            }
        },
        Effect::GainLife { amount, player } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc
        }
        Effect::LoseLife { amount, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            if let Some(x) = target {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::SetTapState {
            target,
            scope: _,
            state: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::RemoveCounter {
            count,
            target,
            counter_type: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ChooseCounterKind { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PutChosenCounter { target, count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Sacrifice {
            target,
            count,
            min_count: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::DiscardCard { target, count: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Mill {
            count,
            target,
            destination: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Scry { count, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PumpAll { .. } => Axes::CONSERVATIVE,
        Effect::DamageAll {
            amount,
            target,
            player_filter,
            damage_source: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            if let Some(x) = player_filter {
                acc = acc.or(scan_player_filter(x, mode));
            }
            acc
        }
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc = acc.or(scan_player_filter(player_filter, mode));
            acc
        }
        Effect::EachPlayerCopyChosen {
            choose_filter,
            min: _,
            max: _,
            copy_modifications: _,
            scale: _,
            choose_scope: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(choose_filter, target_ctx, mode));
            acc
        }
        Effect::DestroyAll {
            target,
            cant_regenerate: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ChangeZone { .. } => Axes::CONSERVATIVE,
        Effect::ChangeZoneAll { .. } => Axes::CONSERVATIVE,
        Effect::Dig {
            player,
            count,
            filter,
            destination: _,
            keep_count: _,
            up_to: _,
            rest_destination: _,
            reveal: _,
            enter_tapped: _,
            source: _,
            keep_count_expr,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            // A dynamic keep-count is a projected-resource read (axis 3): "keep N
            // cards" where N scales with game state feeds the growing-cascade
            // detector exactly like `count`. Classify it identically, not `_`.
            if let Some(kce) = keep_count_expr {
                acc = acc.or(scan_quantity_expr(kce, mode));
            }
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc
        }
        Effect::GainControl { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::GainControlAll { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ControlNextTurn {
            target,
            grant_extra_turn_after: _,
            window: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Attach { attachment, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(attachment, target_ctx, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::UnattachAll { attachment, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(attachment, target_ctx, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Surveil { count, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Fight { target, subject } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(subject, target_ctx, mode));
            acc
        }
        Effect::Bounce {
            target,
            destination: _,
            selection: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::BounceAll {
            target,
            count,
            destination: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            if let Some(x) = count {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc
        }
        Effect::Explore => Axes::NONE,
        Effect::ExploreAll { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc
        }
        Effect::Investigate => Axes::NONE,
        Effect::Tribute { count: _ } => Axes::NONE,
        Effect::TimeTravel => Axes::NONE,
        Effect::BecomeMonarch => Axes::NONE,
        Effect::NoOp => Axes::NONE,
        Effect::Proliferate => Axes::NONE,
        Effect::ProliferateTarget { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Populate => Axes::NONE,
        Effect::Clash => Axes::NONE,
        // CR 701.4a: behold projects no growing resource — it is a boolean
        // reveal-or-choose keyword action.
        Effect::Behold { .. } => Axes::NONE,
        Effect::EndTheTurn => Axes::NONE,
        Effect::EndCombatPhase => Axes::NONE,
        Effect::Vote { .. } => Axes::CONSERVATIVE,
        Effect::SeparateIntoPiles { .. } => Axes::CONSERVATIVE,
        Effect::SwitchPT { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::CopySpell { .. } => Axes::CONSERVATIVE,
        Effect::EpicCopy { .. } => Axes::CONSERVATIVE,
        Effect::CastCopyOfCard {
            target,
            count,
            cost: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            if let Some(x) = count {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc
        }
        Effect::CopyTokenOf { .. } => Axes::CONSERVATIVE,
        Effect::CreateTokenCopyFromPool {
            owner,
            type_filter,
            mv_bound,
            count,
            mv: _,
            selection: _,
            tapped: _,
            enters_attacking: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(owner, target_ctx, mode));
            acc = acc.or(scan_target_filter(type_filter, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(mv_bound, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Myriad => Axes::NONE,
        Effect::Encore => Axes::NONE,
        Effect::CombineHost { host, source: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(host, target_ctx, mode));
            acc
        }
        Effect::ChooseAugmentAndCombineWithHost {
            filter,
            host,
            zones: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc = acc.or(scan_target_filter(host, target_ctx, mode));
            acc
        }
        Effect::Meld {
            source: _,
            partner: _,
            result: _,
            source_filter,
            partner_filter,
            entry: _,
        } => scan_target_filter(source_filter, target_ctx, mode).or(scan_target_filter(
            partner_filter,
            target_ctx,
            mode,
        )),
        Effect::ExileHaunting { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::HideawayConceal { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::CopyTokenBlockingAttacker {
            source_filter,
            owner,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(source_filter, target_ctx, mode));
            acc = acc.or(scan_target_filter(owner, target_ctx, mode));
            acc
        }
        Effect::BecomeCopy { .. } => Axes::CONSERVATIVE,
        Effect::GainActivatedAbilitiesOfTarget {
            target,
            recipient,
            // `scope` is a static compile-time selector of WHICH donor ability
            // categories to snapshot (activated-only vs. all-other); it reads no
            // game state, so it contributes no projected-resource/choice axis.
            scope: _,
            duration,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(recipient, target_ctx, mode));
            if let Some(x) = duration {
                acc = acc.or(scan_duration(x, mode));
            }
            acc
        }
        Effect::ChooseCard { target, choices: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PutCounter {
            count,
            target,
            counter_type: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PutCounterAll {
            count,
            target,
            counter_type: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::MultiplyCounter {
            target,
            counter_type: _,
            multiplier: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::DoublePT {
            target,
            mode: _,
            factor: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::DoublePTAll {
            target,
            mode: _,
            factor: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::MoveCounters {
            source,
            count,
            target,
            counter_type: _,
            mode: _,
            selection: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(source, target_ctx, mode));
            if let Some(x) = count {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Animate { .. } => Axes::CONSERVATIVE,
        Effect::ReturnAsAura { .. } => Axes::CONSERVATIVE,
        Effect::RegisterBending { kind: _ } => Axes::NONE,
        Effect::GenericEffect { .. } => Axes::CONSERVATIVE,
        Effect::Cleanup {
            clear_remembered: _,
            clear_chosen_player: _,
            clear_chosen_color: _,
            clear_chosen_type: _,
            clear_chosen_card: _,
            clear_imprinted: _,
            clear_triggers: _,
            clear_coin_flips: _,
        } => Axes::NONE,
        // CR 732.2a: same split as `Effect::Token`. Exhaustive 5-field destructure,
        // NO `..`. In `LoopFirewall` the produced-mana metric + optional player
        // target descend; `restrictions`/`grants`/`expiry` express no board read.
        Effect::Mana {
            produced,
            target,
            restrictions: _,
            grants: _,
            expiry: _,
        } => match mode {
            ScanMode::Conservative => Axes::CONSERVATIVE,
            ScanMode::LoopFirewall => {
                let mut acc = scan_mana_production(produced, mode);
                if let Some(t) = target {
                    acc = acc.or(scan_target_filter(
                        t,
                        FilterReadContext::SnapshotOrEvent,
                        mode,
                    ));
                }
                acc
            }
        },
        Effect::Discard {
            count,
            target,
            unless_filter,
            filter,
            selection: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            if let Some(x) = unless_filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::Shuffle { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Transform { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::SearchLibrary { .. } => Axes::CONSERVATIVE,
        Effect::SearchOutsideGame {
            filter,
            count,
            reveal: _,
            destination: _,
            source_pool: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::RevealHand {
            target,
            card_filter,
            count,
            selection: _,
            choice_optional: _,
            reveal: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(card_filter, target_ctx, mode));
            if let Some(x) = count {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc
        }
        Effect::RevealFromHand { .. } => Axes::CONSERVATIVE,
        Effect::Reveal { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::RevealTop { player, count: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc
        }
        Effect::ExileTop {
            player,
            count,
            face_down: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::TargetOnly { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Choose { .. } => Axes::CONSERVATIVE,
        Effect::ChooseDamageSource { source_filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(source_filter, target_ctx, mode));
            acc
        }
        Effect::Suspect { target, scope: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Unsuspect { target, scope: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Connive { target, count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::PhaseOut { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PhaseIn { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ForceBlock { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ForceAttack {
            target,
            required_player,
            duration,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(required_player, target_ctx, mode));
            acc = acc.or(scan_duration(duration, mode));
            acc
        }
        Effect::SolveCase => Axes::NONE,
        Effect::BecomePrepared { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::BecomeUnprepared { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::BecomeSaddled { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::BecomeBlocked { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::SetClassLevel { level: _ } => Axes::NONE,
        Effect::CreateDelayedTrigger { .. } => Axes::CONSERVATIVE,
        Effect::AddTargetReplacement { .. } => Axes::CONSERVATIVE,
        Effect::AddRestriction { .. } => Axes::CONSERVATIVE,
        Effect::ReduceNextSpellCost {
            spell_filter,
            amount: _,
        } => {
            let mut acc = Axes::NONE;
            if let Some(x) = spell_filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::GrantNextSpellAbility {
            player,
            spell_filter,
            modifier: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            if let Some(x) = spell_filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::AddPendingETBCounters {
            count,
            counter_type: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        // Continuous-modification carrier: the mods Vec is an UNDESCENDED subtree
        // (no scan_continuous_modification walker exists), so classify
        // CONSERVATIVE — the fail-closed default for undescended subtrees, exactly
        // as every sibling continuous-modification effect (Animate:802,
        // ReturnAsAura:803, GenericEffect:805). Over-read is inert — this effect
        // never resolves standalone (lifted as CastFromZone permission metadata).
        Effect::AddPendingEntersModifications { .. } => Axes::CONSERVATIVE,
        Effect::CreateEmblem { .. } => Axes::CONSERVATIVE,
        Effect::PayCost { .. } => Axes::CONSERVATIVE,
        Effect::CastFromZone { .. } => Axes::CONSERVATIVE,
        Effect::FreeCastFromZones {
            filter,
            count: _,
            max_total_mv: _,
            zones: _,
            exile_instead_of_graveyard: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc
        }
        // The `on_exile` rider is fixed at parse time and only read by the
        // stack-resolution router when the replacement applies — no game-state
        // read happens at scan time, so NONE stays correct.
        Effect::ExileResolvingSpellInsteadOfGraveyard { on_exile: _ } => Axes::NONE,
        Effect::PreventDamage {
            amount_dynamic,
            target,
            damage_source_filter,
            prevention_duration,
            amount: _,
            scope: _,
        } => {
            let mut acc = Axes::NONE;
            if let Some(x) = amount_dynamic {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            if let Some(x) = damage_source_filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            if let Some(x) = prevention_duration {
                acc = acc.or(scan_duration(x, mode));
            }
            acc
        }
        Effect::CreateDamageReplacement { .. } => Axes::CONSERVATIVE,
        Effect::CreateDrawReplacement { replacement_effect } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_effect(replacement_effect, mode));
            acc
        }
        Effect::LoseTheGame { target } => {
            let mut acc = Axes::NONE;
            if let Some(x) = target {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::WinTheGame { target } => {
            let mut acc = Axes::NONE;
            if let Some(x) = target {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::RollDie { .. } => Axes::CONSERVATIVE,
        Effect::FlipCoin { .. } => Axes::CONSERVATIVE,
        Effect::FlipCoins { .. } => Axes::CONSERVATIVE,
        Effect::FlipCoinUntilLose { .. } => Axes::CONSERVATIVE,
        Effect::RingTemptsYou => Axes::NONE,
        Effect::VentureIntoDungeon => Axes::NONE,
        Effect::VentureInto { dungeon: _ } => Axes::NONE,
        Effect::TakeTheInitiative => Axes::NONE,
        Effect::ArrangePlanarDeckTop { .. } => Axes::NONE,
        Effect::Planeswalk => Axes::NONE,
        Effect::OpenAttractions { count: _ } => Axes::NONE,
        Effect::RollToVisitAttractions => Axes::NONE,
        Effect::AssembleContraptions { count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::AssembleContraptionsFromRollDifference => Axes::NONE,
        Effect::CrankContraptions { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ReassembleContraption {
            target,
            control_mode: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::AssembleContraptionOnSprocket {
            target,
            sprocket: _,
            remaining: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ReassembleContraptionOnSprocket {
            target,
            sprocket: _,
            control_mode: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PutSticker {
            target,
            count,
            max_ticket_cost,
            kind: _,
            ticket_cost_payment: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            if let Some(x) = max_ticket_cost {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc
        }
        Effect::ApplySticker {
            target,
            sticker: _,
            pay_ticket: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ProcessRadCounters => Axes::NONE,
        Effect::GrantCastingPermission { .. } => Axes::CONSERVATIVE,
        Effect::ChooseFromZone {
            filter,
            count: _,
            zone: _,
            additional_zones: _,
            zone_owner: _,
            chooser: _,
            up_to: _,
            selection: _,
            constraint: _,
        } => {
            let mut acc = Axes::NONE;
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        // CR 608.2c: `target` is a SINGLE-OBJECT slot — the one recorded card
        // (`SelfRef`, or a single resolution-chain `TrackedSet` pick), written as one
        // `ChosenAttribute::Card` replace-on-rechoose. Not a board census, so it
        // hardcodes `SnapshotOrEvent` (like Token.owner/attach_to + Mana.target). The
        // veto is RELOCATED to obligation (ii): a grown object bearing a
        // RememberCard-carrying ability is non-inert (`object_is_inert` rejects
        // Activated/triggered defs), so a relax can never mint a false certificate.
        Effect::RememberCard { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                target,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        // CR 205.2a: `category` iterates a FIXED set (the 5 colors / card types),
        // NOT the growing class. Effect-level fields destructured no-`..` (mitigation #2:
        // a new census field forces re-audit here via E0027). F2: the inner `action` is
        // matched EXHAUSTIVELY with NO wildcard — a new `ForEachCategoryAction` variant is
        // a compile error until classified here (closes the fail-OPEN `.. => NONE`).
        Effect::ForEachCategory {
            action,
            category: _,
            chooser: _,
        } => match action {
            // The per-category `PutCounter` target is a bounded single-object slot ⇒
            // relaxes via `target_ctx` (SnapshotOrEvent). A board-reading `Typed` filter
            // still self-vetoes inside `scan_target_filter`.
            ForEachCategoryAction::PutCounter { target, .. } => {
                scan_target_filter(target, target_ctx, mode)
            }
            // CR 608.2c: `ExileFromPool` reads a chain-tracked ZONE pool (library /
            // graveyard / exile / revealed cards), DISJOINT from the battlefield growth
            // class — no battlefield target filter to descend ⇒ NONE (behavior unchanged
            // from the former `.. => NONE` residual, now explicit).
            ForEachCategoryAction::ExileFromPool { .. } => Axes::NONE,
        },
        Effect::ChooseObjectsIntoTrackedSet {
            chooser,
            filter,
            min: _,
            max: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(chooser, target_ctx, mode));
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc
        }
        Effect::ChooseAndSacrificeRest {
            choose_filter,
            sacrifice_filter,
            total_power_cap,
            keeper_constraint,
            categories: _,
            chooser_scope: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(choose_filter, target_ctx, mode));
            acc = acc.or(scan_target_filter(sacrifice_filter, target_ctx, mode));
            if let Some(x) = total_power_cap {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            if let Some(KeeperConstraint::ExactCount { count }) = keeper_constraint {
                acc = acc.or(scan_quantity_expr(count, mode));
            }
            acc
        }
        Effect::Exploit { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::GainEnergy { amount } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc
        }
        Effect::GivePlayerCounter {
            count,
            target,
            counter_kind: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::LoseAllPlayerCounters { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ExileFromTopUntil { .. } => Axes::CONSERVATIVE,
        Effect::RevealUntil {
            player,
            filter,
            count,
            enters_under,
            matched_disposition: _,
            kept_destination: _,
            rest_destination: _,
            enter_tapped: _,
            enters_attacking: _,
            kept_optional_to: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            if let Some(x) = enters_under {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        Effect::Discover {
            mana_value_limit,
            player,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(mana_value_limit, mode));
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc
        }
        Effect::Heist {
            target,
            look_count: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::HeistExile => Axes::NONE,
        Effect::Cascade => Axes::NONE,
        Effect::Ripple { count: _ } => Axes::NONE,
        Effect::MiracleCast { cost: _ } => Axes::NONE,
        Effect::MadnessCast { cost: _ } => Axes::NONE,
        Effect::PutAtLibraryPosition { .. } => Axes::CONSERVATIVE,
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            player,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_quantity_expr(life_payment, mode));
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc
        }
        Effect::PutOnTopOrBottom { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::GiftDelivery { kind: _ } => Axes::NONE,
        Effect::Goad { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::GoadAll { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Detain { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::SetRoomDoorLock { target, op: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ExchangeControl { target_a, target_b } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target_a, target_ctx, mode));
            acc = acc.or(scan_target_filter(target_b, target_ctx, mode));
            acc
        }
        Effect::ChangeTargets {
            target,
            forced_to,
            scope: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            if let Some(x) = forced_to {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc
        }
        Effect::Manifest {
            target,
            count,
            enters_under,
            profile: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            if let Some(x) = enters_under {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        Effect::ManifestDread => Axes::NONE,
        Effect::Cloak {
            target,
            count,
            object_source,
            enters_under,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            if let Some(f) = object_source {
                acc = acc.or(scan_target_filter(f, target_ctx, mode));
            }
            if let Some(x) = enters_under {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        Effect::TurnFaceUp { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::TurnFaceDown { target, profile: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::ExtraTurn { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::GrantExtraLoyaltyActivations { amount, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::SkipNextTurn { target, count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::SkipNextStep {
            target,
            count,
            step: _,
            scope: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::AdditionalPhase {
            target,
            count,
            phase: _,
            after: _,
            followed_by: _,
            attacker_restriction: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Double {
            target,
            target_kind: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::RuntimeHandled { handler: _ } => Axes::NONE,
        Effect::Incubate { count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Amass { count, subtype: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Monstrosity { count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Specialize => Axes::NONE,
        Effect::Renown { count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Bolster { count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Adapt { count } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::Learn => Axes::NONE,
        Effect::Forage => Axes::NONE,
        Effect::Harness => Axes::NONE,
        Effect::CollectEvidence { amount: _ } => Axes::NONE,
        Effect::Endure { amount, subject } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc = acc.or(scan_target_filter(subject, target_ctx, mode));
            acc
        }
        Effect::BlightEffect { player, count: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc
        }
        Effect::Seek {
            filter,
            count,
            from_top: _,
            destination: _,
            enter_tapped: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::SetLifeTotal { target, amount } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc
        }
        Effect::ExchangeLifeWithStat { player, stat: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc
        }
        Effect::ExchangeLifeTotals { player_a, player_b } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player_a, target_ctx, mode));
            acc = acc.or(scan_target_filter(player_b, target_ctx, mode));
            acc
        }
        Effect::SetDayNight { to: _ } => Axes::NONE,
        Effect::GiveControl { target, recipient } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(recipient, target_ctx, mode));
            acc
        }
        Effect::RemoveFromCombat { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Conjure { .. } => Axes::CONSERVATIVE,
        Effect::ApplyPerpetual {
            target,
            modification: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::Intensify { amount, scope: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(amount, mode));
            acc
        }
        Effect::DraftFromSpellbook { .. } => Axes::NONE,
        Effect::ChooseCounterAdjustment {
            adjustment: _,
            count,
        } => scan_quantity_expr(count, mode),
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            scan_effect(replacement_effect, mode)
        }
        Effect::ChaosEnsues => Axes::NONE,
        // Field-less self-gathering effect: no target/quantity axes to scan.
        Effect::RedistributeLifeTotals => Axes::NONE,
        Effect::ReverseTurnOrder => Axes::NONE,
        Effect::ChooseOneOf { .. } => Axes::CONSERVATIVE,
        Effect::Unimplemented {
            name: _,
            description: _,
        } => Axes::NONE,
    }
}

fn scan_quantity_ref(x: &QuantityRef, mode: ScanMode) -> Axes {
    match x {
        QuantityRef::HandSize { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::LifeTotal { player } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::GraveyardSize { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::LifeAboveStarting => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        QuantityRef::StartingLifeTotal => Axes::NONE,
        // CR 701.57a: reads a transient game-state scalar (the last discover's
        // mana-value limit); no growing resource, sibling, or projected axis.
        QuantityRef::TriggeringDiscoverValue => Axes::NONE,
        QuantityRef::ObjectCount { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::ObjectCountDistinct {
            filter,
            qualities: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::ObjectCountBySharedQuality {
            filter,
            quality: _,
            aggregate: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::PlayerCount { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_filter(filter, mode));
            acc
        }
        QuantityRef::EventContextPlayerCount { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_player_filter(filter, mode));
            acc
        }
        QuantityRef::CountersOn { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::CountersOnObjects {
            filter,
            counter_type: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::PlayerCounter { scope, kind: _ } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_count_scope(scope));
            acc
        }
        // CR 122.1f + CR 115.1: the target's controller's player-counter total.
        // Target-relative (the chosen object target) and player-counter-
        // projected; conservatively depends on all axes (over-approximation is
        // always safe — it only forces an extra re-scan, never a stale read).
        QuantityRef::TargetControllerCounter { kind: _ } => Axes::CONSERVATIVE,
        QuantityRef::Variable { name: _ } => Axes::NONE,
        QuantityRef::Power { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::Intensity { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::Toughness { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::ObjectManaValue { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::TargetObjectManaValue { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::ObjectColorCount { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::ObjectNameWordCount { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::ObjectTypelineComponentCount { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::ManaSymbolsInManaCost { scope, .. } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        QuantityRef::SelfManaValue => Axes::NONE,
        QuantityRef::Aggregate {
            filter,
            function: _,
            property: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::ControlledByEachPlayer {
            filter,
            aggregate: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::TargetZoneCardCount { zone: _ } => Axes::NONE,
        QuantityRef::Devotion { .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        QuantityRef::DistinctCardTypes { .. } => Axes::CONSERVATIVE,
        QuantityRef::DistinctSubtypes { .. } => Axes::CONSERVATIVE,
        QuantityRef::CardsExiledBySource => Axes::NONE,
        QuantityRef::ExiledCardPower { index: _ } => Axes::NONE,
        QuantityRef::ZoneCardCount {
            filter,
            scope,
            zone: _,
            card_types: _,
        } => {
            let mut acc = Axes::NONE;
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::LiveBoardCensus,
                    mode,
                ));
            }
            acc = acc.or(scan_count_scope(scope));
            acc
        }
        QuantityRef::BasicLandTypeCount { controller, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(controller));
            acc
        }
        QuantityRef::TrackedSetSize => Axes::NONE,
        QuantityRef::FilteredTrackedSetSize {
            filter,
            caused_by: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::TrackedSetAggregate {
            function: _,
            property: _,
            source,
        } => match source {
            // Chain-published set: reads no trigger/sibling context (unchanged).
            TrackedAnaphorSource::ChainSet => Axes::NONE,
            // Reads `state.current_trigger_events` (the triggering event) →
            // event axis true, mirroring `QuantityRef::EventContextAmount` below.
            TrackedAnaphorSource::TriggeringBatch => Axes {
                event: true,
                sibling: false,
                projected: false,
            },
        },
        QuantityRef::ExiledFromHandThisResolution => Axes::NONE,
        QuantityRef::PreviousEffectAmount { .. } => Axes::NONE,
        QuantityRef::LifeLostThisTurn { player } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::PartySize { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::UnspentMana { color: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        QuantityRef::Speed { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::EventContextAmount => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        QuantityRef::AttachmentsOnLeavingObject { controller, .. } => {
            let mut acc = Axes::NONE;
            if let Some(x) = controller {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        QuantityRef::EventContextSourceCostX => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        // CR 700.2: reads the triggering-spell object (same event axis as
        // EventContextSourceCostX and TimesCostPaidThisResolution).
        QuantityRef::EventContextSourceModesChosen => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        QuantityRef::SpellsCastThisTurn { scope, filter } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_count_scope(scope));
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::SnapshotOrEvent,
                    mode,
                ));
            }
            acc
        }
        QuantityRef::EnteredThisTurn { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: true,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::SacrificedThisTurn { player, filter } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::CrimesCommittedThisTurn => Axes::NONE,
        // Controller turn-accumulator: no event/sibling/projected axis (mirrors
        // CrimesCommittedThisTurn / DescendedThisTurn).
        QuantityRef::BendTypesThisTurn => Axes::NONE,
        QuantityRef::LifeGainedThisTurn { player } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::CardsDrawnThisTurn { player } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::BattlefieldEntriesThisTurn { player, filter } => {
            // CR 732.2a: axis-2 self-assertion. This tally is a board-derived
            // AGGREGATE — `record_battlefield_entry` (game/restrictions.rs) APPENDS
            // to `battlefield_entries_this_turn` on every battlefield entry, so a
            // sibling resolution, or a loop cycle that grows the board, changes its
            // value.
            //
            // The engine ALREADY classifies this journal as loop-pumped:
            // `project_out_resources` clears it at analysis/resource.rs:2977 under
            // "CR 400 (zones) / CR 603.6a (ETB) / CR 701.21 (sacrifice) / CR 111
            // (tokens): append-only event journals a loop pumps". A `sibling: false`
            // here contradicted that, and in the unsafe direction: it let the
            // CR 732.2a firewall certify a loop as bounded while a live observer read
            // the growing class. Per the module header above (ADD-1) over-veto is the
            // ONLY permitted error direction (#4603-preserving).
            //
            // Per the ⛔ INVARIANT on the `TargetFilter::Typed` arm of
            // `scan_target_filter`, a board-AGGREGATE caller MUST self-assert
            // `sibling: true` and must NOT delegate its board-read signal to the
            // `Typed` arm, whose `LoopFirewall` relaxation otherwise erases the
            // signal at the two `ability_definition_reads_sibling_mutable_for_loop`
            // scans inside `fire_time_conditions_read_growing_class` —
            // analysis/resource.rs:1699 (trigger `execute` bodies) and :1722
            // (battlefield ability bodies) — neither of which has a `projected` twin
            // in `fire_time_conditions_read_projected_resource`.
            //
            // The CR 603.3b APNAP ordering gate (game/triggers.rs, the
            // `c2_order_independent` term) is the OTHER consumer of this arm and is
            // provably UNAFFECTED: it scans in `ScanMode::Conservative`, where the
            // `Typed`/`Or[Typed]` filter every producer emits already forces
            // `sibling: true` — measured.
            //
            // The `filter` census intent stays `SnapshotOrEvent`: it is matched
            // against a `BattlefieldEntryRecord`, never a live board.
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::LandsPlayedThisTurn { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::TurnsTaken => Axes::NONE,
        QuantityRef::ZoneChangeCountThisTurn {
            filter,
            from: _,
            to: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::ZoneChangeAggregateThisTurn {
            filter,
            from: _,
            to: _,
            function: _,
            property: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::DamageDealtThisTurn {
            source,
            target,
            aggregate: _,
            group_by: _,
            damage_kind: _,
            channel: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_target_filter(
                source,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc = acc.or(scan_target_filter(
                target,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::ChosenNumber => Axes::NONE,
        QuantityRef::AttackedThisTurn { scope, filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_count_scope(scope));
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::LiveBoardCensus,
                    mode,
                ));
            }
            acc
        }
        QuantityRef::DescendedThisTurn => Axes::NONE,
        QuantityRef::LoyaltyAbilitiesActivatedThisTurn { player } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::SpellsCastLastTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        QuantityRef::SpellsCastThisGame { scope, filter } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_count_scope(scope));
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::SnapshotOrEvent,
                    mode,
                ));
            }
            acc
        }
        QuantityRef::CounterAddedThisTurn {
            actor,
            target,
            counters: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_count_scope(actor));
            acc = acc.or(scan_target_filter(
                target,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::CardsDiscardedThisTurn { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::TokensCreatedThisTurn { player, filter } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        QuantityRef::PlayerActionsThisTurn { player, action: _ } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_player_scope(player));
            acc
        }
        QuantityRef::DungeonsCompleted => Axes::NONE,
        QuantityRef::CostXPaid => Axes::NONE,
        QuantityRef::KickerCount => Axes::NONE,
        QuantityRef::AdditionalCostPaymentCount => Axes::NONE,
        QuantityRef::AdditionalCostPaymentCountFor {
            origin: _,
            origin_ordinal: _,
        } => Axes::NONE,
        QuantityRef::ConvokedCreatureCount => Axes::NONE,
        QuantityRef::TimesCostPaidThisResolution => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        QuantityRef::ManaSpentToCast { .. } => Axes::CONSERVATIVE,
        QuantityRef::ColorsInCommandersColorIdentity => Axes::NONE,
        QuantityRef::CommanderCastFromCommandZoneCount => Axes::NONE,
        QuantityRef::CommanderManaValue { owner, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(owner));
            acc
        }
        QuantityRef::DistinctColorsAmongPermanents { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::DistinctCounterKindsAmong { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        QuantityRef::VoteCount { choice_index: _ } => Axes::NONE,
    }
}

fn scan_quantity_expr(x: &QuantityExpr, mode: ScanMode) -> Axes {
    match x {
        QuantityExpr::Ref { qty } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_ref(qty, mode));
            acc
        }
        QuantityExpr::Fixed { value: _ } => Axes::NONE,
        QuantityExpr::DivideRounded {
            inner,
            divisor: _,
            rounding: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(inner, mode));
            acc
        }
        QuantityExpr::Offset { inner, offset: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(inner, mode));
            acc
        }
        QuantityExpr::ClampMin { inner, minimum: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(inner, mode));
            acc
        }
        QuantityExpr::Multiply { inner, factor: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(inner, mode));
            acc
        }
        QuantityExpr::Sum { exprs } => {
            let mut acc = Axes::NONE;
            for x in exprs {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc
        }
        QuantityExpr::UpTo { max } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(max, mode));
            acc
        }
        QuantityExpr::Power { exponent, base: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(exponent, mode));
            acc
        }
        QuantityExpr::Difference { left, right } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(left, mode));
            acc = acc.or(scan_quantity_expr(right, mode));
            acc
        }
        QuantityExpr::Max { exprs } => {
            let mut acc = Axes::NONE;
            for x in exprs {
                acc = acc.or(scan_quantity_expr(x, mode));
            }
            acc
        }
    }
}

fn scan_ability_condition(x: &AbilityCondition, mode: ScanMode) -> Axes {
    match x {
        AbilityCondition::AdditionalCostPaid { subject, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_object_scope(subject));
            acc
        }
        AbilityCondition::AdditionalCostPaidInstead => Axes::NONE,
        AbilityCondition::AlternativeManaCostPaid => Axes::NONE,
        AbilityCondition::EffectOutcome { signal: _ } => Axes::NONE,
        AbilityCondition::EventOutcomeWon => Axes::NONE,
        AbilityCondition::CoinFlipOutcome { result: _ } => Axes::NONE,
        AbilityCondition::WhenYouDo => Axes::NONE,
        AbilityCondition::WasCast { zone: _ } => Axes::NONE,
        AbilityCondition::CastDuringPhase { phases: _ } => Axes::NONE,
        AbilityCondition::CurrentPhaseIs { phases: _ } => Axes::NONE,
        AbilityCondition::CastTimingPermission { permission: _ } => Axes::NONE,
        AbilityCondition::ManaColorSpent {
            color: _,
            minimum: _,
        } => Axes::NONE,
        AbilityCondition::RevealedHasCardType { .. } => Axes::CONSERVATIVE,
        AbilityCondition::ObjectsShareQuality {
            subject,
            reference,
            quality: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                subject,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc = acc.or(scan_target_filter(
                reference,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        AbilityCondition::TargetSharesNameWithOtherExiledThisWay { target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                target,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::SourceEnteredThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        AbilityCondition::CastVariantPaid { subject, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_object_scope(subject));
            acc
        }
        AbilityCondition::CastVariantPaidInstead { variant: _ } => Axes::NONE,
        AbilityCondition::QuantityCheck {
            lhs,
            rhs,
            comparator: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(lhs, mode));
            acc = acc.or(scan_quantity_expr(rhs, mode));
            acc
        }
        AbilityCondition::PreviousEffectAmount {
            rhs,
            comparator: _,
            channel: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(rhs, mode));
            acc
        }
        AbilityCondition::HasMaxSpeed => Axes::NONE,
        AbilityCondition::IsMonarch => Axes::NONE,
        // CR 309.7: controller-state predicate — touches no scan axis.
        AbilityCondition::CompletedDungeon { .. } => Axes::NONE,
        AbilityCondition::IsInitiative => Axes::NONE,
        AbilityCondition::HasCityBlessing => Axes::NONE,
        AbilityCondition::IsRingBearer => Axes::NONE,
        AbilityCondition::TargetHasKeywordInstead { keyword: _ } => Axes::NONE,
        // `subject_slot: _` is a target-slot INDEX selector (CR 608.2c): `Some(n)`
        // tests `filter` against declared chain slot `n` (via
        // `resolve_parent_slot_from_root`), `None` against the local most-recent
        // target. It reroutes WHICH already-declared target the filter reads and
        // introduces no new event/sibling/projected resource — the game-state read
        // is entirely through `filter` (scanned below). Axes-neutral; destructured
        // without `..` so a future read-bearing field forces re-audit.
        AbilityCondition::TargetMatchesFilter {
            filter,
            use_lki: _,
            subject_slot: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::HasObjectTarget => Axes::NONE,
        AbilityCondition::TriggeringSpellTargetsFilter { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::SourceMatchesFilter { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        // CR 615.5: gates on the prevented event's damage source — an event read.
        AbilityCondition::PostReplacementDamageSourceMatchesFilter { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::ZoneChangeObjectMatchesFilter {
            filter,
            origin: _,
            destination: _,
        } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::ControllerControlsMatching { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        AbilityCondition::ControllerControlledMatchingAsCast { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::IsYourTurn => Axes::NONE,
        AbilityCondition::WasStartingPlayer { controller, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(controller));
            acc
        }
        AbilityCondition::SpellCastWithVariantThisTurn { variant: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        AbilityCondition::FirstCombatPhaseOfTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        AbilityCondition::FirstEndStepOfTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        AbilityCondition::ZoneChangedThisWay { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::CostPaidObjectMatchesFilter { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        AbilityCondition::SourceIsTapped => Axes::NONE,
        AbilityCondition::SourceAttachedToCreature => Axes::NONE,
        AbilityCondition::ConditionInstead { inner } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_ability_condition(inner, mode));
            acc
        }
        AbilityCondition::And { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_ability_condition(x, mode));
            }
            acc
        }
        AbilityCondition::Or { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_ability_condition(x, mode));
            }
            acc
        }
        AbilityCondition::Not { condition } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_ability_condition(condition, mode));
            acc
        }
        AbilityCondition::DayNightIsNeither => Axes::NONE,
        AbilityCondition::DayNightIs { state: _ } => Axes::NONE,
        AbilityCondition::NthResolutionThisTurn { n: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        AbilityCondition::SourceLacksKeyword { keyword: _ } => Axes::NONE,
        AbilityCondition::ScopedPlayerMatches { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_filter(filter, mode));
            acc
        }
    }
}

fn scan_guess_subject(x: &GuessSubject, mode: ScanMode) -> Axes {
    match x {
        GuessSubject::CommittedChoice { choice_type: _ } => Axes::NONE,
        GuessSubject::Proposition {
            lhs,
            comparator: _,
            rhs,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(lhs, mode));
            acc = acc.or(scan_quantity_expr(rhs, mode));
            acc
        }
    }
}

fn scan_target_filter(x: &TargetFilter, ctx: FilterReadContext, mode: ScanMode) -> Axes {
    // CR 732.2a firewall census discipline (REQ-1). `LiveBoardCensus`: this CALL
    // SITE counts/tests battlefield membership ⇒ `sibling` is the census's OWN read,
    // injected here independent of the filter's shape (also fixing the latent
    // non-`Typed` board-filter miss, "bug (a)"), never relaxed. `SnapshotOrEvent`:
    // the filter names a target/event/snapshot ⇒ `sibling` only from a genuine
    // board-reading component (a bare `Typed` under `LoopFirewall` relaxes — the
    // coverability gate).
    let base = match ctx {
        FilterReadContext::LiveBoardCensus => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        FilterReadContext::SnapshotOrEvent => Axes::NONE,
    };
    base.or(match x {
        TargetFilter::None => Axes::NONE,
        TargetFilter::Any => Axes::NONE,
        TargetFilter::Player => Axes::NONE,
        TargetFilter::Controller => Axes::NONE,
        TargetFilter::Opponent => Axes::NONE,
        TargetFilter::SelfRef => Axes::NONE,
        // CR 201.5a: a source-relative object ref (the granting object), like
        // SelfRef — no event/sibling/projected resource axis.
        TargetFilter::GrantingObject => Axes::NONE,
        // CR 608.2c: source-relative object ref (concretized to SpecificObject),
        // like SelfRef — no event/sibling/projected resource axis.
        TargetFilter::OriginalSource => Axes::NONE,
        TargetFilter::SourceOrPaired => Axes::NONE,
        // CR 106.1 / CR 119 / CR 122.1: a Typed target filter reads a PROJECTED
        // player resource ONLY via a property/controller that references one
        // (authority: `project_out_resources`, analysis/resource.rs). Pure
        // type/controller predicates read none. `event`/`sibling` stay CONSERVATIVE
        // (byte-preserved) — only the projected axis is refined.
        //
        // ⛔ INVARIANT (CR 732.2a firewall soundness): this arm is the SOLE
        // `sibling: true` source inside `scan_target_filter`. A board-AGGREGATE
        // caller (a color/type-from-board mana metric, a `scan_quantity_ref`
        // `ObjectCount`, an `IsPresent` static condition) MUST self-assert its OWN
        // `sibling: true` literal and only THEN `.or(scan_target_filter(..))` — it
        // must NOT delegate its board-read signal to this `Typed` arm. Two reasons:
        // (a) a non-`Typed` board filter would be missed even today; (b) a future
        // P3 (`sibling: mode == Conservative`) relaxation of this arm would silently
        // turn every delegating aggregate into a false certificate.
        TargetFilter::Typed(tf) => {
            // CR 732.2a: the 3rd mode-divergent arm (with `Effect::Token`,
            // `Effect::Mana`). `event` stays unconditionally true (byte-preserved).
            // Under `Conservative` `sibling` stays true (byte-identical over-veto);
            // under `LoopFirewall` it is precise — `props.sibling` is true only if a
            // property/controller genuinely reads the board (fail-closed), false for
            // a bare type/controller predicate (the canary's untap-all
            // `Typed{Creature}` relaxes, permitting the offer).
            let props = typed_filter_axes(tf, mode);
            Axes {
                event: true,
                sibling: match mode {
                    ScanMode::Conservative => true,
                    ScanMode::LoopFirewall => props.sibling,
                },
                projected: props.projected,
            }
        }
        TargetFilter::Not { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, ctx, mode));
            acc
        }
        TargetFilter::Or { filters } => {
            let mut acc = Axes::NONE;
            for x in filters {
                acc = acc.or(scan_target_filter(x, ctx, mode));
            }
            acc
        }
        TargetFilter::And { filters } => {
            let mut acc = Axes::NONE;
            for x in filters {
                acc = acc.or(scan_target_filter(x, ctx, mode));
            }
            acc
        }
        TargetFilter::StackAbility { controller, .. } => {
            let mut acc = Axes::NONE;
            if let Some(x) = controller {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        TargetFilter::StackSpell => Axes::NONE,
        TargetFilter::SpecificObject { id: _ } => Axes::NONE,
        TargetFilter::SpecificPlayer { id: _ } => Axes::NONE,
        TargetFilter::Neighbor { direction: _ } => Axes::NONE,
        TargetFilter::ScopedPlayer => Axes::NONE,
        TargetFilter::AttachedTo => Axes::NONE,
        TargetFilter::LastCreated => Axes::NONE,
        TargetFilter::LastRevealed => Axes::NONE,
        TargetFilter::CostPaidObject => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::ChosenCard => Axes::NONE,
        TargetFilter::TrackedSet { id: _ } => Axes::NONE,
        TargetFilter::TrackedSetFiltered {
            filter,
            id: _,
            caused_by: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(filter, ctx, mode));
            acc
        }
        TargetFilter::ExiledBySource => Axes::NONE,
        TargetFilter::ExiledCardByIndex { index: _ } => Axes::NONE,
        TargetFilter::TriggeringSpellController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::TriggeringSpellOwner => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::TriggeringPlayer => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::TriggeringSource => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::EventTarget => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::TriggeringSourceController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::ParentTarget => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::ParentTargetSlot { .. } => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::ParentTargetController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::ParentTargetOwner => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::SourceChosenPlayer => Axes::NONE,
        TargetFilter::PlayerWhoChoseLabel { label: _ } => Axes::NONE,
        TargetFilter::OriginalController => Axes::NONE,
        TargetFilter::PostReplacementSourceController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        // CR 615.5: resolves the prevented event's damage source — an event read.
        TargetFilter::PostReplacementDamageSource => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::PostReplacementDamageTarget => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::PostReplacementDamageTargetOwner => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TargetFilter::DefendingPlayer => Axes::NONE,
        TargetFilter::HasChosenName => Axes::NONE,
        TargetFilter::ChosenDamageSource { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            if let Some(f) = filter {
                acc = acc.or(scan_target_filter(f, ctx, mode));
            }
            acc
        }
        TargetFilter::Named { name: _ } => Axes::NONE,
        TargetFilter::Owner => Axes::NONE,
        TargetFilter::AllPlayers => Axes::NONE,
        // CR 615: controller-relative compound recipient — no event/sibling axes.
        TargetFilter::ControllerAndControlledPermanents { .. } => Axes::NONE,
    })
}

fn scan_object_scope(x: &ObjectScope) -> Axes {
    match x {
        ObjectScope::Source => Axes::NONE,
        ObjectScope::Target => Axes::NONE,
        ObjectScope::Recipient => Axes::NONE,
        ObjectScope::EventSource => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        ObjectScope::CostPaidObject => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        ObjectScope::Anaphoric => Axes::NONE,
        ObjectScope::Demonstrative => Axes::NONE,
        // CR 608.2c: per-resolution local (the other revealer's card), resolved
        // by exclusion within this ability's own resolution — no event/sibling
        // axis, like the demonstrative/anaphoric referents.
        ObjectScope::OtherRevealedCard => Axes::NONE,
        ObjectScope::AmassedArmy => Axes::NONE,
        // CR 607.2a: source-persistent exile-pile member read — no event/sibling
        // projected axis (mirrors AmassedArmy).
        ObjectScope::OwnedLinkedExileCard => Axes::NONE,
        ObjectScope::EventTarget => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
    }
}

fn scan_trigger_condition(x: &TriggerCondition, mode: ScanMode) -> Axes {
    match x {
        TriggerCondition::GainedLife { minimum: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::LostLife => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::Descended => Axes::NONE,
        TriggerCondition::ControlsType { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        TriggerCondition::NoSpellsCastLastTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::TwoOrMoreSpellsCastLastTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::DuringPlayersTurn { player } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_filter(player, mode));
            acc
        }
        TriggerCondition::SourceEnteredThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::EchoDue => Axes::NONE,
        TriggerCondition::MinCoAttackers { filter, minimum: _ } => {
            let mut acc = Axes::NONE;
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::LiveBoardCensus,
                    mode,
                ));
            }
            acc
        }
        TriggerCondition::SolveConditionMet => Axes::NONE,
        TriggerCondition::ClassLevelGE { level: _ } => Axes::NONE,
        TriggerCondition::SourceIsHarnessed => Axes::NONE,
        TriggerCondition::AttractionVisitRoll { min: _, max: _ } => Axes::NONE,
        TriggerCondition::WasCast {
            controller, owner, ..
        } => {
            let mut acc = Axes::NONE;
            if let Some(x) = controller {
                acc = acc.or(scan_controller_ref(x));
            }
            if let Some(x) = owner {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        TriggerCondition::WasPlayed => Axes::NONE,
        TriggerCondition::AdditionalCostPaid {
            source: _,
            origin: _,
            origin_ordinal: _,
            variant: _,
            kicker_cost: _,
            min_count: _,
        } => Axes::NONE,
        TriggerCondition::SourceIsAttacking => Axes::NONE,
        TriggerCondition::CastVariantPaid { variant: _ } => Axes::NONE,
        TriggerCondition::CastVariantPaidPersistent { variant: _ } => Axes::NONE,
        TriggerCondition::ActivatedAbilityIsNonMana => Axes::NONE,
        TriggerCondition::DealtDamageBySourceThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::DealtDamageThisTurnBySource { source } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_target_filter(
                source,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::FirstTimeObjectTappedThisTurn => Axes::NONE,
        TriggerCondition::WasType { card_type: _ } => Axes::NONE,
        TriggerCondition::LifeTotalGE { minimum: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::ControlCount { filter, minimum: _ } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        TriggerCondition::ControlsNone { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        TriggerCondition::AttackedThisTurn => Axes::NONE,
        TriggerCondition::FirstCombatPhaseOfTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::CastSpellThisTurn { filter } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::SnapshotOrEvent,
                    mode,
                ));
            }
            acc
        }
        TriggerCondition::QuantityComparison {
            lhs,
            rhs,
            comparator: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(lhs, mode));
            acc = acc.or(scan_quantity_expr(rhs, mode));
            acc
        }
        TriggerCondition::HasMaxSpeed => Axes::NONE,
        TriggerCondition::IsMonarch => Axes::NONE,
        TriggerCondition::IsInitiative => Axes::NONE,
        TriggerCondition::NoMonarch => Axes::NONE,
        TriggerCondition::WasStartingPlayer { controller, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(controller));
            acc
        }
        TriggerCondition::SpellCastWithVariantThisTurn { variant: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::HasCityBlessing => Axes::NONE,
        TriggerCondition::CompletedDungeon { specific: _ } => Axes::NONE,
        TriggerCondition::SourceIsTapped => Axes::NONE,
        TriggerCondition::SourceIsTransformed => Axes::NONE,
        TriggerCondition::SourceIsFaceUp => Axes::NONE,
        TriggerCondition::SourceIsFaceDown => Axes::NONE,
        TriggerCondition::SourceInZone { zone: _ } => Axes::NONE,
        TriggerCondition::CounterAddedThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        // CR 603.3b: Mirrors `CounterAddedThisTurn` (same `counter_added_this_turn`
        // board ledger) — `projected: true`. NOT the tapped sibling's `Axes::NONE`;
        // this condition reads the counter journal, so the coverability/ordering
        // detector must see the projected read (fail-open otherwise).
        TriggerCondition::FirstTimeObjectCountersAddedThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::LostLifeLastTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::DefendingPlayerControlsNone { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        TriggerCondition::TributeNotPaid => Axes::NONE,
        TriggerCondition::CastDuringPhase { phases: _ } => Axes::NONE,
        TriggerCondition::CastTimingPermission { permission: _ } => Axes::NONE,
        TriggerCondition::ManaColorSpent {
            color: _,
            minimum: _,
        } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::ManaSpentCondition { text: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        TriggerCondition::HadCounters { .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        TriggerCondition::ControlsCommander { ownership: _ } => Axes::NONE,
        TriggerCondition::IsRenowned { subject: _ } => Axes::NONE,
        TriggerCondition::HasCounters { .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            filter,
            origin: _,
            destination: _,
        } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::ZoneChangeObjectIsTapped => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TriggerCondition::SourceMatchesFilter { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::EventDamageSourceMatchesFilter { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::EventObjectMatchesFilter { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::DamagedPlayerIsEventSourceOwner => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        TriggerCondition::ChosenLabelIs { label: _ } => Axes::NONE,
        TriggerCondition::AttackersDeclaredCount { .. } => Axes::CONSERVATIVE,
        TriggerCondition::ExceptFirstDrawInDrawStep => Axes::NONE,
        TriggerCondition::PlacedByAbilitySource => Axes::NONE,
        TriggerCondition::TriggeringSpellTargetsFilter { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::TriggeringSpellMatchesFilter { filter } => {
            let mut acc = Axes {
                event: true,
                sibling: false,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        TriggerCondition::And { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_trigger_condition(x, mode));
            }
            acc
        }
        TriggerCondition::Or { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_trigger_condition(x, mode));
            }
            acc
        }
        TriggerCondition::Not { condition } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_trigger_condition(condition, mode));
            acc
        }
    }
}

fn scan_duration(x: &Duration, mode: ScanMode) -> Axes {
    match x {
        Duration::UntilEndOfTurn => Axes::NONE,
        Duration::UntilEndOfCombat => Axes::NONE,
        Duration::UntilNextTurnOf { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        Duration::UntilEndOfNextTurnOf { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        Duration::UntilHostLeavesPlay => Axes::NONE,
        Duration::UntilSourceExilesAnotherCard => Axes::NONE,
        Duration::UntilNextStepOf { player, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_scope(player));
            acc
        }
        Duration::ForAsLongAs { condition } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_static_condition(condition, mode));
            acc
        }
        Duration::Permanent => Axes::NONE,
    }
}

fn scan_static_condition(x: &StaticCondition, mode: ScanMode) -> Axes {
    match x {
        StaticCondition::DevotionGE { .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        StaticCondition::IsPresent { filter } => {
            let mut acc = Axes::NONE;
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::LiveBoardCensus,
                    mode,
                ));
            }
            acc
        }
        StaticCondition::ChosenColorIs { color: _ } => Axes::NONE,
        StaticCondition::ChosenLabelIs { label: _ } => Axes::NONE,
        StaticCondition::QuantityComparison {
            lhs,
            rhs,
            comparator: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(lhs, mode));
            acc = acc.or(scan_quantity_expr(rhs, mode));
            acc
        }
        StaticCondition::HasMaxSpeed => Axes::NONE,
        StaticCondition::SpeedGE { threshold: _ } => Axes::NONE,
        StaticCondition::And { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_static_condition(x, mode));
            }
            acc
        }
        StaticCondition::Or { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_static_condition(x, mode));
            }
            acc
        }
        StaticCondition::Not { condition } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_static_condition(condition, mode));
            acc
        }
        StaticCondition::DayNightIs { state: _ } => Axes::NONE,
        StaticCondition::HasCounters { .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        StaticCondition::CastVariantPaid { variant: _ } => Axes::NONE,
        StaticCondition::RecipientHasCounters { .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        StaticCondition::ClassLevelGE { level: _ } => Axes::NONE,
        StaticCondition::DefendingPlayerControls { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        StaticCondition::SourceAttackingAlone => Axes::NONE,
        StaticCondition::SourceIsAttacking => Axes::NONE,
        StaticCondition::SourceIsBlocking => Axes::NONE,
        StaticCondition::SourceIsBlocked => Axes::NONE,
        StaticCondition::IsMonarch => Axes::NONE,
        StaticCondition::IsInitiative => Axes::NONE,
        StaticCondition::NoMonarch => Axes::NONE,
        StaticCondition::HasCityBlessing => Axes::NONE,
        StaticCondition::CompletedADungeon => Axes::NONE,
        StaticCondition::WasStartingPlayer { controller, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(controller));
            acc
        }
        StaticCondition::SpellCastWithVariantThisTurn { variant: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        StaticCondition::OpponentPoisonAtLeast { count: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        StaticCondition::UnlessPay { .. } => Axes::CONSERVATIVE,
        StaticCondition::Unrecognized { text: _ } => Axes::NONE,
        StaticCondition::DuringYourTurn => Axes::NONE,
        StaticCondition::SharesColorWithMostCommonColorAmongPermanents => Axes::NONE,
        StaticCondition::SourceEnteredThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        StaticCondition::SourceHasDealtDamage => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        StaticCondition::WasCast { zone: _ } => Axes::NONE,
        StaticCondition::IsRingBearer => Axes::NONE,
        StaticCondition::RingLevelAtLeast { level: _ } => Axes::NONE,
        StaticCondition::ControlsCommander { ownership: _ } => Axes::NONE,
        StaticCondition::SourceIsTapped => Axes::NONE,
        StaticCondition::IsTapped { scope, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_object_scope(scope));
            acc
        }
        StaticCondition::SourceIsSaddled => Axes::NONE,
        StaticCondition::SourceControllerEquals { player: _ } => Axes::NONE,
        StaticCondition::SourceIsEquipped => Axes::NONE,
        StaticCondition::SourceIsEnchanted => Axes::NONE,
        StaticCondition::SourceIsMonstrous => Axes::NONE,
        StaticCondition::SourceIsHarnessed => Axes::NONE,
        StaticCondition::SourceAttachedToCreature => Axes::NONE,
        StaticCondition::SourceMatchesFilter { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        StaticCondition::TopOfLibraryMatches { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        StaticCondition::RecipientMatchesFilter { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        StaticCondition::RecipientAttackingOwnerTarget { target: _ } => Axes::NONE,
        StaticCondition::SourceIsPaired => Axes::NONE,
        StaticCondition::SourceInZone { zone: _ } => Axes::NONE,
        StaticCondition::EnchantedIsFaceDown => Axes::NONE,
        StaticCondition::SourceIsFaceUp => Axes::NONE,
        StaticCondition::AdditionalCostPaid => Axes::NONE,
        StaticCondition::CastingAsVariant { variant: _ } => Axes::NONE,
        StaticCondition::None => Axes::NONE,
    }
}

/// Full read-axes of a `TargetFilter::Typed` filter's `controller` + `properties`
/// (CR 106.1 / CR 119 / CR 122.1). `type_filters` are pure card-type predicates
/// (CR 205) and read no player resource, so only the optional `controller` ref and
/// the `properties` vector are scanned (`event` on the `Typed` arm is supplied by
/// the caller). The returned `sibling` is board-reading-property-driven: a bare
/// type/controller predicate yields `sibling:false`, so the caller's `LoopFirewall`
/// relaxation is exact, while a genuine board-reading reference-comparison property
/// keeps `sibling:true` (fail-closed). The prop descent passes `LiveBoardCensus` to
/// `scan_filter_prop`'s nested `scan_target_filter` reads (a bare-`Typed` canary has
/// no such props so is unaffected).
fn typed_filter_axes(tf: &TypedFilter, mode: ScanMode) -> Axes {
    let mut acc = tf
        .controller
        .as_ref()
        .map_or(Axes::NONE, scan_controller_ref);
    for p in &tf.properties {
        acc = acc.or(scan_filter_prop(p, mode));
    }
    acc
}

/// Classify a single `FilterProp` on the three read axes. **Exhaustive with NO
/// `_` wildcard** — a NEW `FilterProp` variant fails to compile here until it is
/// classified (fail-closed to CONSERVATIVE when its read surface is unproven).
/// Every nested-bearing prop recurses the matching sub-scanner so a projected
/// read reached through a property (`PtComparison { value: Ref(LifeTotal) }`,
/// `ControllerMatches { OpponentLostLife }`, `Targets { Typed{..} }`, …) is not
/// lost. The projected-axis authority is `project_out_resources`
/// (analysis/resource.rs): a field is projected iff that fn clears it.
fn scan_filter_prop(x: &FilterProp, mode: ScanMode) -> Axes {
    match x {
        // --- board / object / printed-characteristic leaves: no player resource.
        // Their drift breaks the board-equality gate (item 1), not the item-4 scan.
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::RepresentedByCard
        | FilterProp::WasPlayed
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
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
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::MatchesLastChosenCardPredicate
        | FilterProp::HasSingleTarget
        | FilterProp::Modal
        | FilterProp::NotColor { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        // CR 701.15b/c: goad is a candidate-local designation read; it scans no
        // board/object axis.
        | FilterProp::Goaded
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InTrackedSet { .. }
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::InAnyZone { .. }
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
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
        | FilterProp::SameNameAsParentTarget
        | FilterProp::IsCommander
        // CR 205.3m + CR 903.3: reads commander designation + the candidate's own
        // creature types — a board/object read, no player resource.
        | FilterProp::SharesCreatureTypeWithCommander
        | FilterProp::Other { .. } => Axes::NONE,

        // --- QuantityExpr-bearing: recurse so `Ref(LifeTotal)` / `PlayerCounter`
        // thresholds surface the projected axis (CR 119 / CR 122.1). Finding A:
        // `PtComparison` MUST recurse — "power ≤ your life total" is projected.
        FilterProp::Counters { count, .. } => scan_quantity_expr(count, mode),
        FilterProp::Cmc { value, .. } => scan_quantity_expr(value, mode),
        FilterProp::PtComparison { value, .. } => scan_quantity_expr(value, mode),

        // --- Box<TargetFilter>-bearing: recurse (a nested Typed could be projected).
        FilterProp::CanEnchant { target } => scan_target_filter(target, FilterReadContext::LiveBoardCensus, mode),
        FilterProp::DifferentNameFrom { filter } => scan_target_filter(filter, FilterReadContext::LiveBoardCensus, mode),
        FilterProp::DistinctFrom { reference } => scan_target_filter(reference, FilterReadContext::LiveBoardCensus, mode),
        FilterProp::SharesQuality { reference, .. } => {
            reference
                .as_deref()
                .map_or(Axes::NONE, |r| scan_target_filter(r, FilterReadContext::LiveBoardCensus, mode))
        }
        FilterProp::TargetsOnly { filter } => scan_target_filter(filter, FilterReadContext::LiveBoardCensus, mode),
        FilterProp::Targets { filter } => scan_target_filter(filter, FilterReadContext::LiveBoardCensus, mode),

        // --- Box<PlayerFilter>-bearing: recurse (OpponentLostLife/… is projected).
        FilterProp::ControllerMatches { player } => scan_player_filter(player, mode),

        // --- FilterProp-nesting: recurse.
        FilterProp::AnyOf { props } => {
            let mut acc = Axes::NONE;
            for p in props {
                acc = acc.or(scan_filter_prop(p, mode));
            }
            acc
        }
        FilterProp::Not { prop } => scan_filter_prop(prop, mode),

        // --- ControllerRef-bearing: recurse for self-documentation. Every
        // `scan_controller_ref` outcome is projected:false, so these never lift the
        // projected axis; recursing keeps the classifier honest under future
        // ControllerRef changes.
        FilterProp::Attacking { defender } => {
            defender.as_ref().map_or(Axes::NONE, scan_controller_ref)
        }
        FilterProp::ProtectorMatches { controller } => scan_controller_ref(controller),
        FilterProp::Owned { controller } => scan_controller_ref(controller),
        FilterProp::HasAttachment { controller, .. } => {
            controller.as_ref().map_or(Axes::NONE, scan_controller_ref)
        }
        FilterProp::HasAnyAttachmentOf { controller, .. } => {
            controller.as_ref().map_or(Axes::NONE, scan_controller_ref)
        }
        FilterProp::MostPrevalentCreatureTypeIn { scope, .. } => scan_controller_ref(scope),
        FilterProp::AttackedThisTurn { defender } => {
            defender.as_ref().map_or(Axes::NONE, scan_controller_ref)
        }
        FilterProp::NameMatchesAnyPermanent { controller } => {
            controller.as_ref().map_or(Axes::NONE, scan_controller_ref)
        }

        // --- fail-closed CONSERVATIVE (projected:true):
        // CR 122.1: reads `counter_added_this_turn`, cleared by
        // `project_out_resources` — PROVEN projected.
        FilterProp::CountersPutOnThisTurn { .. } => Axes::CONSERVATIVE,
        // CR 120: runtime eval reads `state.damage_dealt_this_turn` (NOT the object's
        // `damage_marked` — the variant doc is stale), which `project_out_resources`
        // clears and `object_resource_axes_match` does NOT strict-compare (it compares
        // only `damage_marked` + `counters`). A creature dealt damage then regenerated
        // has `damage_marked == 0` yet a persistent journal record, so gate (1) cannot
        // backstop this read — PROVEN projected, fail closed.
        FilterProp::WasDealtDamageThisTurn => Axes::CONSERVATIVE,
        // CR 120.1: reads `state.damage_dealt_this_turn`, the same append-only
        // per-turn journal a loop pumps and `project_out_resources` clears — a
        // projected-resource read, PROVEN projected, fail closed (mirrors the
        // passive `WasDealtDamageThisTurn` arm above).
        FilterProp::DealtDamageThisTurn => Axes::CONSERVATIVE,
        // CR 400 / CR 603.6a: runtime eval reads `state.zone_changes_this_turn`, an
        // append-only event journal a loop pumps, cleared by `project_out_resources`
        // and strict-compared by nothing in gate (1). A flicker/blink loop keeps the
        // net board equal each cycle while the journal grows — PROVEN projected, fail
        // closed.
        FilterProp::ZoneChangedThisTurn { .. } => Axes::CONSERVATIVE,
        // reads `player_last_chose_label`; the backing field is NOT proven to be
        // outside `project_out_resources`'s cleared set, so fail closed.
        FilterProp::ControllerChoseLabel { .. } => Axes::CONSERVATIVE,
    }
}

fn scan_player_filter(x: &PlayerFilter, mode: ScanMode) -> Axes {
    match x {
        PlayerFilter::Controller => Axes::NONE,
        PlayerFilter::Opponent => Axes::NONE,
        PlayerFilter::DefendingPlayer => Axes::NONE,
        PlayerFilter::OpponentLostLife => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        PlayerFilter::OpponentGainedLife => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        PlayerFilter::HasLostTheGame => Axes::NONE,
        // `kind` is a static damage-kind selector (combat/noncombat/any) — not an
        // event-context, sibling, or projected-growth resource — so it carries no
        // axis; only the optional `source` sub-filter contributes.
        PlayerFilter::OpponentDealtDamage {
            kind: _,
            source,
            // A distinct-source-count threshold; carries no scan axis of its own
            // (the source read is already classified via `source` below).
            min_sources: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            if let Some(x) = source {
                acc = acc.or(scan_target_filter(
                    x,
                    FilterReadContext::SnapshotOrEvent,
                    mode,
                ));
            }
            acc
        }
        PlayerFilter::OpponentAttacked {
            subject: _,
            scope: _,
        } => Axes::NONE,
        // CR 508.6: inverse combat relation of `OpponentAttacked` — reads the
        // per-combat attack-declaration ledger and the source's (static)
        // AttachedTo host. Neither is an event-context or projected-growth
        // resource, matching the `OpponentAttacked` / `DefendingPlayer` arms.
        PlayerFilter::OpponentAttackingEnchantedPlayer => Axes::NONE,
        PlayerFilter::All => Axes::NONE,
        PlayerFilter::AllExcept { exclude } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_player_filter(exclude, mode));
            acc
        }
        PlayerFilter::HighestSpeed => Axes::NONE,
        PlayerFilter::ZoneChangedThisWay => Axes::NONE,
        PlayerFilter::PerformedActionThisWay {
            relation: _,
            action: _,
        } => Axes::NONE,
        PlayerFilter::OwnersOfCardsExiledBySource => Axes::NONE,
        PlayerFilter::TriggeringPlayer => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        PlayerFilter::OpponentOtherThanTriggering => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        PlayerFilter::OpponentOfTriggeringPlayer => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        PlayerFilter::OpponentOfTriggeringPlayerNotAttacked => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        PlayerFilter::VotedFor { choice_index: _ } => Axes::NONE,
        PlayerFilter::ParentObjectTargetController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        PlayerFilter::ControlsCount {
            filter,
            count,
            relation: _,
            comparator: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: true,
                projected: false,
            };
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        PlayerFilter::PlayerAttribute {
            attr,
            value,
            relation: _,
            comparator: _,
        } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_quantity_ref(attr, mode));
            acc = acc.or(scan_quantity_expr(value, mode));
            acc
        }
        PlayerFilter::ChosenPlayer { index: _ } => Axes::NONE,
        PlayerFilter::ParentObjectTargetOwner => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
    }
}

fn scan_replacement_condition(x: &ReplacementCondition, mode: ScanMode) -> Axes {
    match x {
        ReplacementCondition::And { conditions } => {
            let mut acc = Axes::NONE;
            for x in conditions {
                acc = acc.or(scan_replacement_condition(x, mode));
            }
            acc
        }
        ReplacementCondition::UnlessControlsSubtype { subtypes: _ } => Axes::NONE,
        ReplacementCondition::UnlessControlsOtherLeq { .. } => Axes::CONSERVATIVE,
        ReplacementCondition::UnlessControlsMatching { filter } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        ReplacementCondition::UnlessControlsCountMatching { filter, minimum: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        ReplacementCondition::UnlessPlayerLifeAtMost { amount: _ } => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        ReplacementCondition::UnlessMultipleOpponents => Axes::NONE,
        ReplacementCondition::UnlessYourTurn => Axes::NONE,
        ReplacementCondition::UnlessQuantity {
            lhs,
            rhs,
            active_player_req,
            comparator: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(lhs, mode));
            acc = acc.or(scan_quantity_expr(rhs, mode));
            if let Some(x) = active_player_req {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        ReplacementCondition::OnlyIfQuantity {
            lhs,
            rhs,
            active_player_req,
            comparator: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(lhs, mode));
            acc = acc.or(scan_quantity_expr(rhs, mode));
            if let Some(x) = active_player_req {
                acc = acc.or(scan_controller_ref(x));
            }
            acc
        }
        ReplacementCondition::HasMaxSpeed => Axes::NONE,
        ReplacementCondition::CastViaEscape => Axes::NONE,
        ReplacementCondition::CastVariantPaid { variant: _ } => Axes::NONE,
        ReplacementCondition::CastFromZone { zone: _ } => Axes::NONE,
        ReplacementCondition::EnteredFromZone {
            origin_constraint: _,
            cast_origin: _,
        } => Axes::NONE,
        ReplacementCondition::YouAttackedThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        ReplacementCondition::OpponentDamagedThisTurn => Axes {
            event: false,
            sibling: false,
            projected: true,
        },
        ReplacementCondition::CastViaKicker {
            variant: _,
            kicker_cost: _,
        } => Axes::NONE,
        ReplacementCondition::SourceTappedState { tapped: _ } => Axes::NONE,
        ReplacementCondition::DealtDamageThisTurnBySource { source } => {
            let mut acc = Axes {
                event: false,
                sibling: false,
                projected: true,
            };
            acc = acc.or(scan_target_filter(
                source,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
            acc
        }
        ReplacementCondition::EventSourceControlledBy { controller, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_controller_ref(controller));
            acc
        }
        ReplacementCondition::EffectCausedDiscard => Axes::NONE,
        ReplacementCondition::OnlyExtraTurn => Axes::NONE,
        ReplacementCondition::TokenSubtypeMatches { subtypes: _ } => Axes::NONE,
        ReplacementCondition::TokenCoreTypeMatches { core_types: _ } => Axes::NONE,
        ReplacementCondition::FirstTokenCreationEachTurn { player: _ } => Axes::NONE,
        ReplacementCondition::ExceptFirstDrawInDrawStep => Axes::NONE,
        ReplacementCondition::IfControlsMatching { filter, minimum: _ } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(
                filter,
                FilterReadContext::LiveBoardCensus,
                mode,
            ));
            acc
        }
        ReplacementCondition::ClassLevelGE { level: _ } => Axes::NONE,
        ReplacementCondition::DuringUntapStep => Axes::NONE,
        ReplacementCondition::DuringDrawStep { .. } => Axes::NONE,
        ReplacementCondition::ControllerControlsSource {
            source: _,
            controller: _,
        } => Axes::NONE,
        ReplacementCondition::Unrecognized { text: _ } => Axes::NONE,
    }
}

fn scan_player_scope(x: &PlayerScope) -> Axes {
    match x {
        PlayerScope::Controller => Axes::NONE,
        PlayerScope::ScopedPlayer => Axes::NONE,
        PlayerScope::Target => Axes::NONE,
        PlayerScope::Opponent { aggregate: _ } => Axes::NONE,
        PlayerScope::AllPlayers { exclude, .. } => {
            let mut acc = Axes::NONE;
            if let Some(x) = exclude {
                acc = acc.or(scan_player_scope(x));
            }
            acc
        }
        PlayerScope::RecipientController => Axes::NONE,
        PlayerScope::DefendingPlayer => Axes::NONE,
        PlayerScope::ParentObjectTargetController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        PlayerScope::SourceChosenPlayer => Axes::NONE,
        // CR 513.1: turn-agnostic end-step deadline reached via the
        // `UntilNextStepOf` duration walk — a pure timing referent, no axes.
        PlayerScope::AnyTurn => Axes::NONE,
    }
}

fn scan_controller_ref(x: &ControllerRef) -> Axes {
    match x {
        ControllerRef::You => Axes::NONE,
        ControllerRef::Opponent => Axes::NONE,
        ControllerRef::ScopedPlayer => Axes::NONE,
        ControllerRef::TargetPlayer => Axes::NONE,
        // CR 109.4: TargetOpponent is a target-player slot with opponent-only
        // legality; it is runtime-read-identical to TargetPlayer (the scope
        // restriction is enforced at target selection, not a walker axis).
        ControllerRef::TargetOpponent => Axes::NONE,
        ControllerRef::ParentTargetController => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        ControllerRef::ParentTargetOwner => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        ControllerRef::DefendingPlayer => Axes::NONE,
        ControllerRef::ChosenPlayer { index: _ } => Axes::NONE,
        ControllerRef::SourceChosenPlayer => Axes::NONE,
        ControllerRef::TriggeringPlayer => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        ControllerRef::EnchantedPlayer => Axes::NONE,
        // CR 102.1: a live read of `state.active_player` — no event/sibling axis.
        ControllerRef::ActivePlayer => Axes::NONE,
    }
}

fn scan_count_scope(x: &CountScope) -> Axes {
    match x {
        CountScope::Controller => Axes::NONE,
        CountScope::Owner => Axes::NONE,
        CountScope::ScopedPlayer => Axes::NONE,
        CountScope::SourceChosenPlayer => Axes::NONE,
        CountScope::All => Axes::NONE,
        CountScope::Opponents => Axes::NONE,
    }
}

// ---------------------------------------------------------------------------
// Public classification API (consumed by `game::triggers` ordering and
// `analysis::resource` coverability). Each is a thin projection of one axis.
// ---------------------------------------------------------------------------

/// Axis 3: does this resolved ability (and its chain/conditions) read a
/// projected player-level resource or journal? (`analysis::resource` item 4.)
pub(crate) fn ability_reads_projected_resource(ability: &ResolvedAbility) -> bool {
    resolved_ability_axes(ability, ScanMode::Conservative).projected
}

/// Axis 1: does this resolved ability read the concrete triggering-event /
/// cost-paid-object context? (CR 603.4; `game::triggers` ordering.)
pub(crate) fn ability_uses_event_context(ability: &ResolvedAbility) -> bool {
    resolved_ability_axes(ability, ScanMode::Conservative).event
}

/// Axis 2: does this resolved ability read a source/recipient or board-scoped
/// mutable aggregate a sibling copy could change? (CR 603.3b; `game::triggers`
/// C2 distinct-event auto-resolve gate — the Rubblebelt Rioters / Orcish
/// Siegemaster exclusion.)
pub(crate) fn ability_reads_sibling_mutable(ability: &ResolvedAbility) -> bool {
    resolved_ability_axes(ability, ScanMode::Conservative).sibling
}

/// Axis 3 on a bare trigger fire-time `condition` (CR 603.4 intervening-if) —
/// the off-stack scan surface (`analysis::resource` item 5).
pub(crate) fn trigger_condition_reads_projected_resource(condition: &TriggerCondition) -> bool {
    scan_trigger_condition(condition, ScanMode::Conservative).projected
}

/// Axis 3 on a condition-gated static's `condition` (CR 604.1 / CR 613.1) — the
/// dormant-static off-stack scan surface.
pub(crate) fn static_condition_reads_projected_resource(condition: &StaticCondition) -> bool {
    scan_static_condition(condition, ScanMode::Conservative).projected
}

/// Axis 3 on a replacement effect's `condition`/body (CR 614.1) — the
/// off-stack replacement scan surface.
pub(crate) fn replacement_condition_reads_projected_resource(
    condition: &ReplacementCondition,
) -> bool {
    scan_replacement_condition(condition, ScanMode::Conservative).projected
}

/// Axis 3 on a bare `AbilityCondition` (resolution-time branch selector).
pub(crate) fn ability_condition_reads_projected_resource(condition: &AbilityCondition) -> bool {
    scan_ability_condition(condition, ScanMode::Conservative).projected
}

/// Axis 3 on a transient `Duration::ForAsLongAs` condition (CR 604.1) — the
/// `transient_continuous_effects` off-stack scan surface.
pub(crate) fn duration_reads_projected_resource(duration: &Duration) -> bool {
    scan_duration(duration, ScanMode::Conservative).projected
}

// ---------------------------------------------------------------------------
// Axis-2 (sibling-mutable) off-stack read surface — the object-growth firewall
// (`analysis::resource::loop_states_cover_modulo_object_growth`, PR-7 Phase 4a).
// Mirrors the projected-resource accessors above but projects `.sibling` (the
// board-scoped mutable-aggregate axis, CR 603.3b): "reads a source/recipient or
// board aggregate a sibling copy could mutate" IS "reads the inert growth set
// |G|" (coarsely — the sibling axis subsumes grown-id specificity, so it is a
// fail-safe over-approximation of the CR 613.1b object-growth cover bar). Each
// helper is a thin `.sibling` projection of an existing exhaustive scanner, so a
// new read-bearing AST field forces classification once, in that scanner.
// ---------------------------------------------------------------------------

/// Full read-axes of an `AbilityDefinition` (the def-level analogue of
/// [`resolved_ability_axes`]). Exhaustive no-`..` destructure — a future field
/// fails to compile until classified. `cost` is bound read-free here because the
/// object-growth cost surface is scanned separately by
/// `analysis::resource::cost_surface_references_growing_class` (§5.4).
fn ability_definition_axes(def: &AbilityDefinition, mode: ScanMode) -> Axes {
    let AbilityDefinition {
        // ---- read-bearing ----
        effect,
        sub_ability,
        else_ability,
        duration,
        condition,
        multi_target,
        target_constraints,
        modal,
        mode_abilities,
        repeat_for,
        announced_x,
        player_scope,
        starting_with,
        target_chooser,
        repeat_until,
        // ---- conservative-when-present: inner cost/filter payloads the walk does
        //      not descend into, each able to express a board-scoped read ----
        unless_pay,
        distribute,
        cost_reduction,
        // ---- read-free: cost scanned separately (§5.4), announce-time metadata,
        //      flags, and tags — none express a resolution-time dynamic read ----
        kind: _,
        cost: _,
        description: _,
        target_prompt: _,
        activation_restrictions: _,
        activator_filter: _,
        activation_zone: _,
        ability_tag: _,
        optional_targeting: _,
        optional: _,
        optional_for: _,
        target_choice_timing: _,
        min_x_value: _,
        cant_be_copied: _,
        forward_result: _,
        target_selection_mode: _,
        sub_link: _,
        iteration_kind_binding: _,
    } = def;

    let mut acc = scan_effect(effect, mode);
    if let Some(sub) = sub_ability {
        acc = acc.or(ability_definition_axes(sub, mode));
    }
    if let Some(else_branch) = else_ability {
        acc = acc.or(ability_definition_axes(else_branch, mode));
    }
    if let Some(duration) = duration {
        acc = acc.or(scan_duration(duration, mode));
    }
    if let Some(condition) = condition {
        acc = acc.or(scan_ability_condition(condition, mode));
    }
    // CR 601.2b: the announce-time-locked definition of X is a live board read,
    // merely read earlier (at announcement) than a resolution-time slot.
    if let Some(announced_x) = announced_x {
        acc = acc.or(scan_quantity_expr(announced_x, mode));
    }
    if let Some(MultiTargetSpec { min, max }) = multi_target {
        acc = acc.or(scan_quantity_expr(min, mode));
        if let Some(max) = max {
            acc = acc.or(scan_quantity_expr(max, mode));
        }
    }
    for c in target_constraints {
        acc = acc.or(scan_target_selection_constraint(c, mode));
    }
    if let Some(modal) = modal {
        acc = acc.or(scan_modal_choice(modal, mode));
    }
    for m in mode_abilities {
        acc = acc.or(ability_definition_axes(m, mode));
    }
    if let Some(qty) = repeat_for {
        acc = acc.or(scan_quantity_expr(qty, mode));
    }
    if let Some(ps) = player_scope {
        acc = acc.or(scan_player_filter(ps, mode));
    }
    if let Some(sw) = starting_with {
        acc = acc.or(scan_controller_ref(sw));
    }
    if let Some(chooser) = target_chooser {
        acc = acc.or(scan_target_filter(
            chooser,
            FilterReadContext::SnapshotOrEvent,
            mode,
        ));
    }
    if let Some(ru) = repeat_until {
        acc = acc.or(scan_repeat_continuation(ru, mode));
    }
    // Conservative fail-closed for present-but-undescended cost/filter payloads:
    // an `unless pay {1} for each artifact`, a divide/distribute filter, or a
    // per-condition cost reduction can each express a board-scoped read.
    if unless_pay.is_some() || distribute.is_some() || cost_reduction.is_some() {
        acc = acc.or(Axes::CONSERVATIVE);
    }
    acc
}

/// Axis 2 on a def-level `AbilityDefinition` (trigger `execute` bodies, every
/// `obj.abilities` def regardless of `kind` [S5], granted-ability bodies, and the
/// pending/delayed store bodies).
pub(crate) fn ability_definition_reads_sibling_mutable(def: &AbilityDefinition) -> bool {
    ability_definition_axes(def, ScanMode::Conservative).sibling
}

/// Axis 2 on a bare trigger fire-time `condition` (CR 603.4 intervening-if).
pub(crate) fn trigger_condition_reads_sibling_mutable(condition: &TriggerCondition) -> bool {
    scan_trigger_condition(condition, ScanMode::Conservative).sibling
}

/// Axis 2 on a condition-gated static's `condition` (CR 604.1 / CR 613.1).
pub(crate) fn static_condition_reads_sibling_mutable(condition: &StaticCondition) -> bool {
    scan_static_condition(condition, ScanMode::Conservative).sibling
}

/// Axis 2 on a replacement effect's `condition` (CR 614.1).
pub(crate) fn replacement_condition_reads_sibling_mutable(
    condition: &ReplacementCondition,
) -> bool {
    scan_replacement_condition(condition, ScanMode::Conservative).sibling
}

/// Axis 2 on a transient `Duration::ForAsLongAs` condition (CR 604.1).
pub(crate) fn duration_reads_sibling_mutable(duration: &Duration) -> bool {
    scan_duration(duration, ScanMode::Conservative).sibling
}

/// Axis 2 on any cost surface (§5.4 / Finding-2): EXHAUSTIVE `AbilityCost` match,
/// NO `_`. The five `QuantityExpr`-bearing variants route through
/// [`scan_quantity_expr`]; the three nested containers recurse; `EffectCost` routes
/// to [`scan_effect`]; every fixed/bounded/structural variant is read-free (a new
/// variant fails to compile until classified). Board-referencing cost *keywords*
/// (Affinity/Convoke/…) are IMPLICIT — they carry no scannable `QuantityExpr`, so
/// they are classified separately by [`keyword_cost_reads_growing_class`].
pub(crate) fn ability_cost_references_sibling_mutable(cost: &AbilityCost) -> bool {
    scan_ability_cost(cost, ScanMode::Conservative).sibling
}

/// Axis 2 on a bare `QuantityRef` — the dynamic cost multiplier
/// (`dynamic_count: Option<QuantityRef>`) carried by CR 601.2f cost-modification
/// statics (`StaticMode::ModifyCost` / `StaticMode::ReduceAbilityCost`). Thin
/// `.sibling` projection of the exhaustive [`scan_quantity_ref`] scanner, so a
/// board-reading `ObjectCount` "for each X you control" multiplier is caught by the
/// object-growth cost firewall.
pub(crate) fn quantity_ref_references_sibling_mutable(qty: &QuantityRef) -> bool {
    scan_quantity_ref(qty, ScanMode::Conservative).sibling
}

fn scan_ability_cost(cost: &AbilityCost, mode: ScanMode) -> Axes {
    match cost {
        AbilityCost::ManaDynamic { quantity } => scan_quantity_expr(quantity, mode),
        AbilityCost::PayLife { amount } => scan_quantity_expr(amount, mode),
        AbilityCost::PayEnergy { amount } => scan_quantity_expr(amount, mode),
        AbilityCost::PaySpeed { amount } => scan_quantity_expr(amount, mode),
        AbilityCost::Discard {
            count,
            filter: _,
            selection: _,
            self_scope: _,
        } => scan_quantity_expr(count, mode),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => costs
            .iter()
            .fold(Axes::NONE, |acc, c| acc.or(scan_ability_cost(c, mode))),
        AbilityCost::PerCounter {
            counter: _,
            target,
            base,
        } => scan_target_filter(target, FilterReadContext::SnapshotOrEvent, mode)
            .or(scan_ability_cost(base, mode)),
        AbilityCost::EffectCost { effect } => scan_effect(effect, mode),
        // Fixed / bounded / structural costs: no dynamic board read (a
        // board-reading tap/exile aggregate that varies the *reduction* is caught
        // by the cost-keyword classifier, not here).
        AbilityCost::Mana { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => Axes::NONE,
    }
}

/// §5.4 item (1) — cost-KEYWORD family. Does casting or activating an object that
/// carries `kw` incur a cost whose MAGNITUDE or PAYABILITY is a function of a
/// battlefield/graveyard object quantity — i.e. the cost either (a) scales down by a
/// board/graveyard count, or (b) taps/sacrifices/exiles a member of a board or
/// graveyard object class? Such an IMPLICIT (keyword-driven) cost reads the inert
/// growth set |G| and breaks the fixed-cost extrapolation the object-growth cover
/// relies on (CR 732.2a / §6 keystone: a cast-affordability the `ResourceVector`
/// does not model).
///
/// EXHAUSTIVE no-`_` match on `Keyword` (the repo's no-wildcard scan doctrine): a
/// new `Keyword` variant is a compile break here until classified. Over-approximation
/// is fail-CLOSED — an over-broad `true` only suppresses a loop certification
/// (soundness-preserving); a missed `false` would falsely certify an unbounded loop.
/// When in doubt, `true`.
///
/// TRUE arms (grep-verified CR): Affinity (CR 702.41a, {1} less per matching
/// permanent); the tap-a-board-aggregate keywords — Convoke (CR 702.51a),
/// Improvise (CR 702.126a), Conspire (CR 702.78a), Crew (CR 702.122a), Saddle
/// (CR 702.171a), Station (CR 702.184a), Teamwork (CR 702.194a), Waterbend
/// (CR 701.67), Harmonize (CR 702.180a, taps a creature and reduces by its power);
/// Delve (CR 702.66a, exile graveyard cards); Craft (CR 702.167a, exile
/// battlefield/graveyard materials); the sacrifice-for-reduction keywords — Emerge
/// (CR 702.119a) and Offering (CR 702.48a, reduce by the sacrificed permanent's
/// mana value); the sacrifice-a-board-permanent additional costs — Bargain
/// (CR 702.166a) and Casualty (CR 702.153a); and Assist (CR 702.132a, another
/// player funds the generic mana the summed `ResourceVector` per CR 106.1 cannot
/// attribute — fail-closed).
///
/// Undaunted (CR 702.125a) is SAFE — it reduces by the OPPONENT count (CR 119 player
/// axis), never a board object class, so it cannot read |G|. Every combat/evasion/
/// characteristic keyword, every fixed-mana or self/hand cost keyword, and every
/// ETB/triggered mechanic (whose board reads, if any, are caught by the §5.3a
/// trigger/replacement firewall, not the cost surface) is SAFE.
pub(crate) fn keyword_cost_reads_growing_class(kw: &Keyword) -> bool {
    match kw {
        // (a)/(b): the casting/activation cost reads a battlefield/graveyard object
        // class — a scaling reduction or a tap/sacrifice/exile board aggregate.
        Keyword::Affinity(_)
        | Keyword::Convoke
        | Keyword::Improvise
        | Keyword::Conspire
        | Keyword::Crew { .. }
        | Keyword::Saddle(_)
        | Keyword::Station
        | Keyword::Teamwork(_)
        | Keyword::Waterbend
        | Keyword::Harmonize(_)
        | Keyword::Delve
        | Keyword::Craft { .. }
        | Keyword::Emerge(_)
        | Keyword::Offering(_)
        | Keyword::Bargain
        | Keyword::Casualty(_)
        | Keyword::Assist => true,

        Keyword::Disguise(DisguiseCost::Reduced { .. }) => true,

        // SAFE: no casting/activation cost that reads a growing board/graveyard class.
        Keyword::Flying
        | Keyword::FirstStrike
        | Keyword::DoubleStrike
        | Keyword::Trample
        | Keyword::TrampleOverPlaneswalkers
        | Keyword::Deathtouch
        | Keyword::Lifelink
        | Keyword::Vigilance
        | Keyword::Haste
        | Keyword::Reach
        | Keyword::Defender
        | Keyword::Menace
        | Keyword::Indestructible
        | Keyword::Hexproof
        | Keyword::HexproofFrom(_)
        | Keyword::Shroud
        | Keyword::Flash
        | Keyword::Fear
        | Keyword::Intimidate
        | Keyword::Skulk
        | Keyword::Shadow
        | Keyword::Horsemanship
        | Keyword::Wither
        | Keyword::Infect
        | Keyword::Afflict(_)
        | Keyword::StartingIntensity(_)
        | Keyword::Prowess
        | Keyword::Undying
        | Keyword::Persist
        | Keyword::Cascade
        | Keyword::Exalted
        | Keyword::Flanking
        | Keyword::Evolve
        | Keyword::Extort
        | Keyword::Exploit
        | Keyword::Explore
        | Keyword::Ascend
        | Keyword::StartYourEngines
        | Keyword::Dredge(_)
        | Keyword::Modular(_)
        | Keyword::Renown(_)
        | Keyword::Graft(_)
        | Keyword::Fabricate(_)
        | Keyword::Annihilator(_)
        | Keyword::Bushido(_)
        | Keyword::Frenzy(_)
        | Keyword::Tribute(_)
        | Keyword::Soulbond
        | Keyword::BandsWithOther(_)
        | Keyword::Unearth(_)
        | Keyword::Devoid
        | Keyword::Changeling
        | Keyword::Phasing
        | Keyword::Battlecry
        | Keyword::Decayed
        | Keyword::Unleash
        | Keyword::Riot
        | Keyword::Afterlife(_)
        | Keyword::Enchant(_)
        | Keyword::EtbCounter { .. }
        | Keyword::Reconfigure(_)
        | Keyword::LivingWeapon
        | Keyword::JobSelect
        | Keyword::TotemArmor
        | Keyword::Bestow(_)
        | Keyword::Embalm(_)
        | Keyword::Eternalize(_)
        | Keyword::Fading(_)
        | Keyword::Vanishing(_)
        | Keyword::Protection(_)
        | Keyword::Kicker(_)
        | Keyword::Cycling(_)
        | Keyword::Typecycling { .. }
        | Keyword::Flashback(_)
        | Keyword::Retrace
        | Keyword::Ward(_)
        | Keyword::Equip(_)
        | Keyword::Landwalk(_)
        | Keyword::Rampage(_)
        | Keyword::Absorb(_)
        | Keyword::Partner(_)
        | Keyword::Companion(_)
        | Keyword::CommanderNinjutsu(_)
        | Keyword::Ninjutsu(_)
        | Keyword::Sneak(_)
        | Keyword::Mutate(_)
        | Keyword::Escape(_)
        | Keyword::Morph(_)
        | Keyword::Megamorph(_)
        | Keyword::Madness(_)
        | Keyword::Disguise(DisguiseCost::Mana(_))
        | Keyword::Mayhem(_)
        | Keyword::Suspend { .. }
        | Keyword::Blitz(_)
        | Keyword::Disturb(_)
        | Keyword::Foretell(_)
        | Keyword::Miracle(_)
        | Keyword::Plot(_)
        | Keyword::Gift(_)
        | Keyword::Outlast(_)
        | Keyword::Dash(_)
        | Keyword::Warp(_)
        | Keyword::Devour(_)
        | Keyword::Offspring(_)
        | Keyword::Splice { .. }
        | Keyword::Sunburst
        | Keyword::Champion(_)
        | Keyword::Training
        | Keyword::Augment
        | Keyword::Aftermath
        | Keyword::JumpStart
        | Keyword::Cipher
        | Keyword::Transmute(_)
        | Keyword::Transfigure(_)
        | Keyword::Cleave(_)
        | Keyword::Undaunted
        | Keyword::Paradigm
        | Keyword::Replicate(_)
        | Keyword::Awaken { .. }
        | Keyword::ForMirrodin
        | Keyword::MoreThanMeetsTheEye(_)
        | Keyword::Freerunning(_)
        | Keyword::Increment
        | Keyword::Firebending(_)
        | Keyword::Specialize(_)
        | Keyword::Escalate(_)
        | Keyword::Recover(_)
        | Keyword::Fuse
        | Keyword::Unknown(_)
        | Keyword::Amplify(_)
        | Keyword::Backup(_)
        | Keyword::Banding
        | Keyword::Bloodthirst(_)
        | Keyword::Buyback(_)
        | Keyword::Compleated
        | Keyword::CumulativeUpkeep(_)
        | Keyword::Daybound
        | Keyword::Demonstrate
        | Keyword::Dethrone
        | Keyword::Discover(_)
        | Keyword::DoubleTeam
        | Keyword::Echo(_)
        | Keyword::Encore(_)
        | Keyword::Enlist
        | Keyword::Entwine(_)
        | Keyword::Epic
        | Keyword::Evoke(_)
        | Keyword::Fortify(_)
        | Keyword::Gravestorm
        | Keyword::Haunt
        | Keyword::Hideaway(_)
        | Keyword::Impending { .. }
        | Keyword::Ingest
        | Keyword::LevelUp(_)
        | Keyword::LivingMetal
        | Keyword::Melee
        | Keyword::Mentor
        | Keyword::Mobilize(_)
        | Keyword::Myriad
        | Keyword::Nightbound
        | Keyword::Overload(_)
        | Keyword::Poisonous(_)
        | Keyword::Prototype { .. }
        | Keyword::Provoke
        | Keyword::Prowl(_)
        | Keyword::Ravenous
        | Keyword::ReadAhead
        | Keyword::Rebound
        | Keyword::Reinforce { .. }
        | Keyword::Ripple(_)
        | Keyword::Scavenge(_)
        | Keyword::Soulshift(_)
        | Keyword::Spectacle(_)
        | Keyword::SplitSecond
        | Keyword::Spree
        | Keyword::Squad(_)
        | Keyword::Storm
        | Keyword::Surge(_)
        | Keyword::Totem
        | Keyword::Toxic(_)
        | Keyword::WebSlinging(_) => false,
    }
}

/// §5.4 item (1) — granted-keyword cost family. A runtime-granted cost keyword
/// (`ContinuousModification::AddKeyword`) or a granted keyword whose cost is
/// derived from board state (`AddKeywordWithDerivedCost`) reaches the same
/// affordability hole as a printed one. Every other modification is not a
/// cost-keyword grant (read-free on THIS axis; its board reads, if any, are caught
/// by the §5.3a effect-body firewall).
pub(crate) fn modification_grants_growing_cost_keyword(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::AddKeyword { keyword } => keyword_cost_reads_growing_class(keyword),
        // A derived-cost keyword grant is board-state-driven by construction ⇒
        // conservatively a |G| reader.
        ContinuousModification::AddKeywordWithDerivedCost { .. } => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// CR 732.2a object-growth firewall scanners (LoopFirewall mode only).
//
// These are the P2 walkers that make the `Effect::Token`/`Effect::Mana` blankets
// DESCEND. They are reached exclusively through the `*_for_loop` /
// `continuous_modification_reads_*` entry points below, which the
// `analysis::resource` firewall calls under `loop_detection.samples()`. Nothing
// in the `Conservative`-mode walk (the CR 603.3b gate and every other consumer)
// touches them, so `LoopDetectionMode::Off` is byte-identical by construction.
// ---------------------------------------------------------------------------

/// A token's `power`/`toughness` (CR 208). A dynamic `Quantity` P/T reads its
/// `QuantityExpr`; a fixed or `*`-placeholder P/T reads nothing here.
fn scan_pt_value(pt: &PtValue, mode: ScanMode) -> Axes {
    match pt {
        PtValue::Fixed(_) => Axes::NONE,
        PtValue::Variable(_) => Axes::NONE,
        PtValue::Quantity(q) => scan_quantity_expr(q, mode),
    }
}

/// CR 732.2a: does a keyword carried by a created token READ a growing
/// board/graveyard class? Payload SHAPE alone is unsound (Convoke/Delve/Improvise/
/// Bargain/Station are UNIT variants that read the board), so the COST-read axis
/// delegates to the shipped exhaustive semantic authority
/// [`keyword_cost_reads_growing_class`] (17 tap/sacrifice/exile/scale keywords),
/// `.or()` a descent of the few keyword PAYLOADS that carry a scannable
/// `QuantityExpr` / `TargetFilter` / `AbilityCost`. EXHAUSTIVE, NO `_` wildcard —
/// a new payload-bearing keyword fails to compile until classified.
fn scan_keyword(kw: &Keyword, mode: ScanMode) -> Axes {
    // Axis-2 cost surface: fully exhaustive & fail-closed on new variants.
    let cost_read = if keyword_cost_reads_growing_class(kw) {
        Axes {
            event: false,
            sibling: true,
            projected: false,
        }
    } else {
        Axes::NONE
    };
    let payload_read = match kw {
        // scannable payloads (the only keywords whose parameter can read the board)
        Keyword::Mobilize(q) | Keyword::Firebending(q) => scan_quantity_expr(q, mode),
        Keyword::Enchant(tf) => scan_target_filter(tf, FilterReadContext::SnapshotOrEvent, mode),
        Keyword::CumulativeUpkeep(c) | Keyword::Escalate(c) => scan_ability_cost(c, mode),
        // payload types with no scanner that can transitively express a board read
        // ⇒ fail-closed CONSERVATIVE (a filter / cost-wrapper we do not descend).
        Keyword::HexproofFrom(_)
        | Keyword::Affinity(_)
        | Keyword::Craft { .. }
        | Keyword::Protection(_)
        | Keyword::Companion(_)
        | Keyword::Gift(_)
        | Keyword::Ward(_)
        | Keyword::Bestow(_)
        | Keyword::Embalm(_)
        | Keyword::Eternalize(_)
        | Keyword::Escape(_)
        | Keyword::Evoke(_)
        | Keyword::Echo(_)
        | Keyword::Buyback(_)
        | Keyword::Cycling(_)
        | Keyword::Flashback(_) => Axes::CONSERVATIVE,
        // Every other keyword carries a read-free payload (unit / u32 / String /
        // ManaCost / value tag): it reads nothing on any axis here. Its cost-read,
        // if any, is already captured by `cost_read` above.
        Keyword::Flying
        | Keyword::FirstStrike
        | Keyword::DoubleStrike
        | Keyword::Trample
        | Keyword::TrampleOverPlaneswalkers
        | Keyword::Deathtouch
        | Keyword::Lifelink
        | Keyword::Vigilance
        | Keyword::Haste
        | Keyword::Reach
        | Keyword::Defender
        | Keyword::Menace
        | Keyword::Indestructible
        | Keyword::Hexproof
        | Keyword::Shroud
        | Keyword::Flash
        | Keyword::Fear
        | Keyword::Intimidate
        | Keyword::Skulk
        | Keyword::Shadow
        | Keyword::Horsemanship
        | Keyword::Wither
        | Keyword::Infect
        | Keyword::Afflict(_)
        | Keyword::StartingIntensity(_)
        | Keyword::Prowess
        | Keyword::Undying
        | Keyword::Persist
        | Keyword::Cascade
        | Keyword::Exalted
        | Keyword::Flanking
        | Keyword::Evolve
        | Keyword::Extort
        | Keyword::Exploit
        | Keyword::Explore
        | Keyword::Ascend
        | Keyword::StartYourEngines
        | Keyword::Dredge(_)
        | Keyword::Modular(_)
        | Keyword::Renown(_)
        | Keyword::Fabricate(_)
        | Keyword::Annihilator(_)
        | Keyword::Bushido(_)
        | Keyword::Frenzy(_)
        | Keyword::Tribute(_)
        | Keyword::Soulbond
        | Keyword::Unearth(_)
        | Keyword::Convoke
        | Keyword::Waterbend
        | Keyword::Delve
        | Keyword::Devoid
        | Keyword::Changeling
        | Keyword::Phasing
        | Keyword::Battlecry
        | Keyword::Decayed
        | Keyword::Unleash
        | Keyword::Riot
        | Keyword::Afterlife(_)
        | Keyword::EtbCounter { .. }
        | Keyword::Reconfigure(_)
        | Keyword::LivingWeapon
        | Keyword::JobSelect
        | Keyword::TotemArmor
        | Keyword::Fading(_)
        | Keyword::Vanishing(_)
        | Keyword::Kicker(_)
        | Keyword::Equip(_)
        | Keyword::Landwalk(_)
        | Keyword::Rampage(_)
        | Keyword::Absorb(_)
        | Keyword::Crew { .. }
        | Keyword::Partner(_)
        | Keyword::Ninjutsu(_)
        | Keyword::CommanderNinjutsu(_)
        | Keyword::Prowl(_)
        | Keyword::Morph(_)
        | Keyword::Megamorph(_)
        | Keyword::Mayhem(_)
        | Keyword::Madness(_)
        | Keyword::Miracle(_)
        | Keyword::Dash(_)
        | Keyword::Emerge(_)
        | Keyword::Harmonize(_)
        | Keyword::Foretell(_)
        | Keyword::Mutate(_)
        | Keyword::Disturb(_)
        | Keyword::Disguise(_)
        | Keyword::Blitz(_)
        | Keyword::Overload(_)
        | Keyword::Spectacle(_)
        | Keyword::Surge(_)
        | Keyword::Encore(_)
        | Keyword::Casualty(_)
        | Keyword::Entwine(_)
        | Keyword::Outlast(_)
        | Keyword::Scavenge(_)
        | Keyword::Reinforce { .. }
        | Keyword::Fortify(_)
        | Keyword::Prototype { .. }
        | Keyword::Plot(_)
        | Keyword::Offspring(_)
        | Keyword::Impending { .. }
        | Keyword::LevelUp(_)
        | Keyword::Banding
        | Keyword::BandsWithOther(_)
        | Keyword::Epic
        | Keyword::Fuse
        | Keyword::Gravestorm
        | Keyword::Haunt
        | Keyword::Hideaway(_)
        | Keyword::Improvise
        | Keyword::Ingest
        | Keyword::Melee
        | Keyword::Mentor
        | Keyword::Myriad
        | Keyword::Provoke
        | Keyword::Rebound
        | Keyword::Retrace
        | Keyword::Ripple(_)
        | Keyword::SplitSecond
        | Keyword::Storm
        | Keyword::Suspend { .. }
        | Keyword::Totem
        | Keyword::Warp(_)
        | Keyword::Sneak(_)
        | Keyword::WebSlinging(_)
        | Keyword::Discover(_)
        | Keyword::Spree
        | Keyword::Ravenous
        | Keyword::Daybound
        | Keyword::Nightbound
        | Keyword::Enlist
        | Keyword::ReadAhead
        | Keyword::Compleated
        | Keyword::Conspire
        | Keyword::Demonstrate
        | Keyword::Dethrone
        | Keyword::DoubleTeam
        | Keyword::LivingMetal
        | Keyword::Poisonous(_)
        | Keyword::Bloodthirst(_)
        | Keyword::Amplify(_)
        | Keyword::Graft(_)
        | Keyword::Devour(_)
        | Keyword::Toxic(_)
        | Keyword::Saddle(_)
        | Keyword::Teamwork(_)
        | Keyword::Soulshift(_)
        | Keyword::Backup(_)
        | Keyword::Squad(_)
        | Keyword::Typecycling { .. }
        | Keyword::Splice { .. }
        | Keyword::Bargain
        | Keyword::Sunburst
        | Keyword::Champion(_)
        | Keyword::Training
        | Keyword::Assist
        | Keyword::Augment
        | Keyword::Aftermath
        | Keyword::JumpStart
        | Keyword::Cipher
        | Keyword::Transmute(_)
        | Keyword::Transfigure(_)
        | Keyword::Recover(_)
        | Keyword::Cleave(_)
        | Keyword::Undaunted
        | Keyword::Paradigm
        | Keyword::Station
        | Keyword::Replicate(_)
        | Keyword::Awaken { .. }
        | Keyword::ForMirrodin
        | Keyword::MoreThanMeetsTheEye(_)
        | Keyword::Freerunning(_)
        | Keyword::Increment
        | Keyword::Specialize(_)
        | Keyword::Offering(_)
        | Keyword::Unknown(_) => Axes::NONE,
    };
    cost_read.or(payload_read)
}

/// CR 106.1/106.7/109.1: the produced-mana metric of an `Effect::Mana`. Two
/// distinct sibling-read paths (R1): a COUNT-DRIVEN metric's board read (if any)
/// lives entirely inside its `count` (self-guarded by `scan_quantity_ref`'s
/// `ObjectCount` arm), while a color/type-FROM-BOARD aggregate must self-assert
/// its OWN `sibling:true` (see the invariant at `scan_target_filter`'s `Typed`
/// arm). EXHAUSTIVE over all 15 variants, NO `_` wildcard.
fn scan_mana_production(p: &ManaProduction, mode: ScanMode) -> Axes {
    match p {
        // COUNT-DRIVEN: any board read lives inside `count`; NO own sibling literal.
        ManaProduction::Colorless { count }
        | ManaProduction::AnyOneColor { count, .. }
        | ManaProduction::AnyCombination { count, .. }
        | ManaProduction::ChosenColor { count, .. }
        | ManaProduction::OpponentLandColors { count }
        | ManaProduction::AnyInCommandersColorIdentity { count, .. } => {
            scan_quantity_expr(count, mode)
        }
        // SCOPED-OBJECT (Omnath, Locus of All): a SINGLE scoped object's colors,
        // NOT a board aggregate — the scope's own read surface is the sole sibling
        // source (CR 202.2c). NO own sibling literal.
        ManaProduction::AnyCombinationOfObjectColors { count, scope } => {
            scan_quantity_expr(count, mode).or(scan_object_scope(scope))
        }
        // ⛔ BOARD-AGGREGATE (color/type-from-board): self-assert OWN `sibling:true`
        // (R1 — mirror `scan_quantity_ref`; must NOT delegate the board read to the
        // `Typed` arm of `scan_target_filter`).
        ManaProduction::DistinctColorsAmongPermanents { filter } => Axes {
            event: false,
            sibling: true,
            projected: false,
        }
        .or(scan_target_filter(
            filter,
            FilterReadContext::LiveBoardCensus,
            mode,
        )),
        ManaProduction::AnyOneColorAmongPermanents { count, filter, .. } => Axes {
            event: false,
            sibling: true,
            projected: false,
        }
        .or(scan_quantity_expr(count, mode))
        .or(scan_target_filter(
            filter,
            FilterReadContext::LiveBoardCensus,
            mode,
        )),
        ManaProduction::AnyTypeProduceableBy { count, land_filter } => Axes {
            event: false,
            sibling: true,
            projected: false,
        }
        .or(scan_quantity_expr(count, mode))
        .or(scan_target_filter(
            land_filter,
            FilterReadContext::LiveBoardCensus,
            mode,
        )),
        // CR 106.3: reads the triggering `ManaAdded` event (event axis).
        ManaProduction::TriggerEventManaType => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        // read-free: fixed colors / fixed pre-specified combinations read nothing.
        ManaProduction::Fixed { .. }
        | ManaProduction::Mixed { .. }
        | ManaProduction::ChoiceAmongCombinations { .. } => Axes::NONE,
        // no walker for `LinkedExileScope` ⇒ fail-closed CONSERVATIVE.
        ManaProduction::ChoiceAmongExiledColors { .. } => Axes::CONSERVATIVE,
    }
}

/// CR 613.1 + CR 732.2a: does a continuous modification READ a mutable board
/// aggregate (`sibling`) or a projected player resource (`projected`)? EXHAUSTIVE
/// over all 53 `ContinuousModification` variants, NO `_` wildcard — a new variant
/// fails to compile until classified. `mode` is threaded to the granted-ability
/// descent (`GrantAbility`) so a token body inside a grant is classified in the
/// same mode. The AST is finite and acyclic, so the mutual recursion terminates.
fn scan_continuous_modification(m: &ContinuousModification, mode: ScanMode) -> Axes {
    match m {
        // descend the dynamic P/T / dynamic-keyword / enter-counter QuantityExpr (8)
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. } => {
            scan_quantity_expr(value, mode)
        }
        ContinuousModification::AddCounterOnEnter { count, .. } => scan_quantity_expr(count, mode),
        // descend the granted keyword (2, B4 — routes through the same authority)
        ContinuousModification::AddKeyword { keyword }
        | ContinuousModification::RemoveKeyword { keyword } => scan_keyword(keyword, mode),
        // descend a granted ability body (GrantAbility). Presence of Gond's aura
        // grants a `{T}: Create ...` activated ability whose token body reads
        // nothing sibling — descending is what lets the firewall NOT over-veto it.
        ContinuousModification::GrantAbility { definition } => {
            ability_definition_axes(definition, mode)
        }
        // fail-closed CONSERVATIVE: inner payloads with no walker (9). `GrantTrigger`
        // carries a `TriggerDefinition` (a `TriggerMode`, not a `TriggerCondition`)
        // that is outside the scanner's traversal closure — conservative is the
        // documented fail-safe (over-veto = missed offer). A full `TriggerDefinition`
        // walker is a follow-up.
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::GrantTrigger { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        // upstream #6009 (Sakashima): copy-layer "retain this object's own abilities"
        // — same class as the RetainPrinted* siblings (no inner walker) ⇒ fail-closed.
        | ContinuousModification::RetainAllOtherAbilitiesFromSource => Axes::CONSERVATIVE,
        // read-free (33): static structural mods (name/type/color/anthem/chosen-
        // attribute/copy-time) read no growing aggregate. An anthem `Add/SetPower`
        // applies to a growing class but READS nothing.
        ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        // CR 612.8 / CR 613.1c: a literal-name text-changing effect reads no board
        // aggregate or projected resource (sibling of `SetChosenName`).
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => Axes::NONE,
    }
}

/// LoopFirewall-mode axis-2 (`sibling`) on a def-level `AbilityDefinition` (trigger
/// `execute` bodies, every functioning `obj.abilities` def, granted-ability bodies)
/// — the CR 732.2a object-growth firewall's DESCENDING body scan (§P0-e row 2).
pub(crate) fn ability_definition_reads_sibling_mutable_for_loop(def: &AbilityDefinition) -> bool {
    ability_definition_axes(def, ScanMode::LoopFirewall).sibling
}

/// CR 613.1 + CR 732.2a: does a live continuous modification READ a mutable board
/// aggregate (axis-2 `sibling`)? Consumed by the `analysis::resource` `:1539`
/// modification firewall descent.
pub(crate) fn continuous_modification_reads_sibling_mutable(m: &ContinuousModification) -> bool {
    scan_continuous_modification(m, ScanMode::LoopFirewall).sibling
}

/// CR 106.1 / CR 119 / CR 122.1 + CR 732.2a: does a live continuous modification
/// READ a projected player resource (axis-3 `projected`)? Load-bearing (M9): the
/// projected-resource firewall has NO modification scan, so this `:1539` descent is
/// the sole guard against a projected-reading modification (a
/// `SetDynamicPower{Ref(LifeTotal)}` anthem).
pub(crate) fn continuous_modification_reads_projected_resource(m: &ContinuousModification) -> bool {
    scan_continuous_modification(m, ScanMode::LoopFirewall).projected
}

/// CR 732.2a + BLOCKER-1: the census discipline for `scan_effect`'s effect-TARGET
/// filter reads (the `FilterReadContext` a `scan_target_filter(target, _, mode)`
/// call inside `scan_effect` passes). INVERTED default — census unless PROVEN
/// inert-checkable. Every effect-target read is `LiveBoardCensus` (veto = safe)
/// UNLESS the effect is in the small, explicit, pinned proven-inert exception set;
/// an unclassified / future cardinality-driving `Effect` (the next
/// `EachSourceDealsDamage`) lands `LiveBoardCensus` = fail-CLOSED (missed offer,
/// never false offer; #4603-preserving). Mirrors `effect_resolution_choice_freedom`:
/// EXHAUSTIVE `match e`, NO `_` wildcard — a NEW `Effect` variant fails to compile
/// until placed in one of the two arms (the natural, safe choice is the census
/// group). There is NO `_ => SnapshotOrEvent` anywhere in this path.
///
/// MAJOR-1 mode-gate: under `Conservative` the census-vs-relax decision does NOT
/// exist — effect targets pass a fixed `SnapshotOrEvent` (base NONE), BYTE-IDENTICAL
/// to the old scan (non-`Typed` target → NONE; `Typed` target → the `Typed` arm's
/// `Conservative` `sibling:true`). The inverted census default applies ONLY under
/// `LoopFirewall`. This touches ONLY effect targets — the self-asserting
/// `QuantityRef` census arms pass `LiveBoardCensus` directly and are unchanged in
/// both modes.
///
/// Pinned proven-inert exception set = `{SetTapState}` ONLY (obligation (ii):
/// tap/untap of an INERT grown token feeds no drivability — `object_is_inert`
/// (resource.rs) guarantees no `{T}` ability for an untap to enable — and the stable
/// host's tap state is part of the certified recurrence via `board_covers`). The two
/// damage aggregates (`EachSourceDealsDamage` / `EachDealsDamageEqualToPower`) fall
/// to census automatically (their `.sources` cardinality DRIVES escalating player
/// damage), resolving BLOCKER-1 with zero per-site hand-classification.
fn effect_target_ctx(e: &Effect, mode: ScanMode) -> FilterReadContext {
    // MAJOR-1: mode-gate the ROUTING. Under `Conservative` effect targets pass a
    // fixed `SnapshotOrEvent` (byte-identity). The inverted census default is
    // `LoopFirewall`-only.
    if mode != ScanMode::LoopFirewall {
        return FilterReadContext::SnapshotOrEvent;
    }
    match e {
        // ── GENUINELY-CENSUS effects (CR 732.2a / CR 120.3): a target filter is a
        // MASS POPULATION read — enumerated over EVERY matching battlefield object (an
        // AllX/Each/aggregate slot, `target_filter()==None`), so its read SCALES with the
        // growing class ⇒ fail-CLOSED census. obligation-(ii) does NOT license relaxing
        // these: a loop growing INERT tokens a DamageAll reads has all-inert grown objects
        // (grown_objects_are_inert==true) yet the census read still escalates ⇒ only the
        // sibling veto catches it. Pinned EXACTLY by `census_tag_set_is_exactly_enumerated`
        // (guard#3). Defense-in-depth: PumpAll/DamageEachPlayer/ChangeZoneAll are census-
        // tagged even though their scan_effect arm is CONSERVATIVE/non-scanning today, so a
        // future descent into their mass filter cannot silently relax.
        Effect::EachSourceDealsDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::CounterAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::GainControlAll { .. }
        | Effect::PumpAll { .. }
        | Effect::BounceAll { .. }
        | Effect::UnattachAll { .. }
        | Effect::ExploreAll { .. }
        | Effect::PutCounterAll { .. }
        | Effect::DoublePTAll { .. }
        | Effect::GoadAll { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        // R1 (CR 701.60a): Suspect/Unsuspect scope:All is a mass-population battlefield
        // read (`target_filter()`==None; `suspect.rs` enumerates `state.battlefield`,
        // "like DestroyAll") ⇒ census — its read SCALES with the growing class. scope:Single
        // is a single announced target (a2), relaxed in the single-object group below. The
        // two scopes are exhaustive for Suspect/Unsuspect (EffectScope = {Single, All}).
        // Fail-CLOSED: over-vetoes the Absolving Lammasu mass-unsuspect shortcut OFFER
        // (missed offer, never a false certificate).
        | Effect::Suspect { scope: EffectScope::All, .. }
        | Effect::Unsuspect { scope: EffectScope::All, .. }
        // ── F1-CLASS DUAL-MODE MASS-BATTLEFIELD RESOLVERS (P3-B round-2): each has a
        // resolver mode that, when the ability carries NO explicit object target,
        // enumerates the battlefield (or all phased-in/-out permanents) and applies the
        // effect to EVERY matching object — a MASS-POPULATION read that SCALES with the
        // growing class, exactly like the DestroyAll/PumpAll group above. There is NO
        // static field discriminating the announced-single mode from the mass mode
        // (it's the resolution-time `ability.targets.is_empty()` / `ParentTarget`
        // branch), so per the fail-closed framework the WHOLE variant censuses:
        // over-vetoing the bounded/announced mode is the SAFE direction (a missed
        // shortcut offer, never a false certificate). These sat in the SnapshotOrEvent
        // relax `|`-chain below (the same silent-miss class as R1's Suspect{All}) until
        // the resolver audit surfaced them; each `scan_effect` arm routes its filter
        // through this `target_ctx` (BecomeCopy is `Axes::CONSERVATIVE` today, so its tag
        // is defense-in-depth parity with PumpAll/ChangeZoneAll). Pinned by guard#3.
        //   CR 702.26 (Phasing): `phase_out.rs` mass "phase out/in each permanent you
        //     control" iterates `battlefield_phased_in_ids()` / `state.battlefield`.
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        //   CR 611.2c (continuous-effect affected set fixed at inception): `gain_
        //     activated_abilities.rs` grants to EACH matching battlefield object
        //     ("each Horror you control"); `become_copy.rs` copies onto a mass
        //     recipient set ("Shards you control", CR 707.2).
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::BecomeCopy { .. }
        //   CR 708.2 / CR 708.2a (face-down permanents): `resolved_battlefield_object_
        //     ids` (effects/mod.rs) falls through to a battlefield mass scan for a
        //     non-targeted "turn each matching creature face up/down" (Illithid
        //     Harvester).
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        //   CR 701.10 (Double): `counters.rs` `resolve_defined_or_targets` mass-scans
        //     `battlefield_phased_in_ids()` for a non-targeted "double the counters on
        //     each matching permanent" when `ability.targets.is_empty()`.
        | Effect::MultiplyCounter { .. }
        //   CR 707.2 + CR 509.1g + CR 506.3e (team-lead override of the combat-scoped
        //     relax): `copy_token_blocking.rs` UNCONDITIONALLY enumerates
        //     `zone_object_ids(Battlefield).filter(matches source_filter)` and creates one
        //     token copy per matching attacker — a mass read that GROWS the board. The
        //     combat-fixed population is NOT sound across multi-combat loops: CR 508.1
        //     extra-combat engines re-declare attackers each combat, so a board grown by
        //     prior iterations yields MORE attackers ⇒ unbounded copies. Its scan_effect
        //     arm routes `source_filter` through this `target_ctx`, so the tag is
        //     runtime-live (unlike CopyTokenOf, which is already scan_effect-CONSERVATIVE).
        | Effect::CopyTokenBlockingAttacker { .. } => FilterReadContext::LiveBoardCensus,
        // ── OBLIGATION-(ii)-PROVEN NON-ESCALATION EXCEPTION — the SOLE census-role slot
        // classified Snapshot. `SetTapState` ("untap/tap all matching", scope All) is
        // census-ROLE, but tapping/untapping is STATE-CONVERGENT (idempotent per object,
        // adds no ability/counter/keyword): an untapped grown token is still inert
        // (object_is_inert, resource.rs:1380-1399) AND its tap flag is compared by
        // board_covers, so the read cannot escalate. NOT a general (b)-license — a specific
        // proven exception. Destructured no-`..` so a new field forces re-audit. Pinned by
        // `obligation_ii_census_exception_is_exactly_settapstate` (A7').
        Effect::SetTapState {
            target: _,
            scope: _,
            state: _,
        } => FilterReadContext::SnapshotOrEvent,
        // ── SINGLE-OBJECT / bounded-selection slots (author's contract restored, CR 732.2a):
        // the target selects ONE object/player (announced single target (a2), or a bounded
        // ref (a1): owner/recipient/attach host/chooser/self/remembered/player), or a
        // bounded selection from a non-battlefield pool — an O(1) read that does NOT scale
        // with the growing class. A board-reading Typed filter still self-vetoes via
        // `scan_target_filter`'s `base.or(shape)` props.sibling (c). NO wildcard: a new
        // Effect variant is a compile error until classified census-vs-snapshot.
        Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::RemoveCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::ChangeZone { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::Explore
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        // R1 (CR 701.60a): only the scope:Single Suspect/Unsuspect relaxes — a single
        // announced target (a2). scope:All is a mass battlefield read, census-tagged above.
        | Effect::Suspect { scope: EffectScope::Single, .. }
        | Effect::Unsuspect { scope: EffectScope::Single, .. }
        | Effect::Connive { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        // upstream #6070 (Susan Foreman): reorders the PLANAR DECK top (Planechase),
        // not a battlefield population ⇒ not a live-board census (relax).
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategory { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => FilterReadContext::SnapshotOrEvent,
    }
}

// ---------------------------------------------------------------------------
// F3 (CR 732.2a): census-completeness PARTITION. The INDEPENDENT oracle that
// cross-checks `effect_target_ctx`. The gap that let R1's Suspect{All} bug hide
// (a census-ROLE slot silently in `effect_target_ctx`'s generic relax `|`-chain)
// is closed here: EVERY `Effect` variant is classified EXHAUSTIVELY (NO wildcard)
// into `Census` (mass battlefield population, scales with growth => fail-closed)
// or `Relax(reason)`, so a new variant fails the exhaustive match (a compile
// error in every test / `clippy --all-targets` build, both run by CI) until a
// human assigns its role, and `census_partition_agrees_with_effect_target_ctx`
// asserts the two functions' `Census` sets are byte-identical. This oracle is
// `#[cfg(test)]` guard infrastructure (like the other census guards below), not
// runtime code.
//
// The discriminating property is BATTLEFIELD-MASS-POPULATION, NOT
// `target_filter()==None`. That distinction is load-bearing in BOTH directions:
//   * `Effect::UnattachAll` is `target_filter()==Some` yet census-ROLE (its
//     `target` is a mass population filter), so census is NOT a subset of
//     `target_filter()==None`; we classify by scaling ROLE, mirroring
//     `effect_target_ctx`.
//   * `Dig`/`Seek`/`SearchOutsideGame`/`RevealHand` are `target_filter()==None`
//     yet correctly RELAXED - they read library/hand/exile pools DISJOINT from
//     the battlefield growth class (`RelaxReason::ZoneDisjoint`). A naive
//     "`target_filter()==None` => census" rule would fail-CLOSED on all of them.
// The `Relax` reason sub-tags are documentation (auditor-facing); only the
// `Census`/`Relax` boundary is guard-enforced.
// ---------------------------------------------------------------------------

/// Why a non-census `Effect` read does NOT scale with the battlefield growth
/// class. Documentation granularity - only the `Census`/`Relax` split is
/// guard-enforced (see module note above).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelaxReason {
    /// Reads a NON-battlefield pool (library / hand / graveyard / exile /
    /// outside-game / stack), disjoint from the growing battlefield class.
    ZoneDisjoint,
    /// The obligation-(ii)-proven state-convergent exception: `SetTapState`
    /// (tap/untap is idempotent per object and adds no ability/counter/keyword).
    SetTapStateException,
    /// A single announced/bounded target, a fixed-category iteration, or a
    /// player-/self-only read with no battlefield population filter - O(1) in
    /// the growth class.
    BoundedOrNoPopulation,
}

/// The census-vs-relax ROLE of an `Effect`'s target-filter read. Mirrors
/// `effect_target_ctx`'s `LiveBoardCensus`/`SnapshotOrEvent` decision as an
/// independent, exhaustively-classified oracle (F3).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CensusRole {
    /// Mass battlefield population read - enumerated over every matching object,
    /// scales with the growing class => fail-CLOSED census. EXACTLY the 28
    /// `effect_target_ctx` `LiveBoardCensus` members.
    Census,
    Relax(RelaxReason),
}

/// F3: exhaustive per-variant census-role classification (NO wildcard). A new
/// `Effect` variant is a compile error until placed in one of the arms below,
/// converting the F1 silent-miss into a forced, reasoned decision for the whole
/// CLASS. Cross-checked against `effect_target_ctx` by
/// `census_partition_agrees_with_effect_target_ctx`.
#[cfg(test)]
fn effect_census_role(e: &Effect) -> CensusRole {
    match e {
        // -- CENSUS (28): verbatim mirror of `effect_target_ctx`'s LiveBoardCensus
        // arm - mass battlefield population reads that scale with growth.
        Effect::EachSourceDealsDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::CounterAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::GainControlAll { .. }
        | Effect::PumpAll { .. }
        | Effect::BounceAll { .. }
        | Effect::UnattachAll { .. }
        | Effect::ExploreAll { .. }
        | Effect::PutCounterAll { .. }
        | Effect::DoublePTAll { .. }
        | Effect::GoadAll { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::Suspect {
            scope: EffectScope::All,
            ..
        }
        | Effect::Unsuspect {
            scope: EffectScope::All,
            ..
        }
        // -- F1-CLASS DUAL-MODE MASS-BATTLEFIELD RESOLVERS (P3-B round-2): mirror of the
        // new `effect_target_ctx` LiveBoardCensus members. Each has a resolver mode that,
        // absent an explicit object target, enumerates the battlefield and applies to
        // EVERY matching object (scales with growth). No static discriminator ⇒ whole
        // variant censuses, fail-closed. CR 702.26 (PhaseOut/PhaseIn phasing mass);
        // CR 611.2c + CR 707.2 (GainActivated/BecomeCopy mass continuous-effect set);
        // CR 708.2 (TurnFaceUp/TurnFaceDown mass face-up/down via
        // resolved_battlefield_object_ids); CR 701.10 (MultiplyCounter mass counter
        // doubling). Cross-checked byte-identical with effect_target_ctx by
        // census_partition_agrees_with_effect_target_ctx.
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::BecomeCopy { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::MultiplyCounter { .. }
        // CR 707.2 + CR 509.1g (team-lead override): `copy_token_blocking.rs` creates one
        // token copy per matching attacker over an UNCONDITIONAL battlefield scan (grows
        // the board); unsound across CR 508.1 multi-combat loops. Mirror of the new
        // effect_target_ctx census member.
        | Effect::CopyTokenBlockingAttacker { .. } => CensusRole::Census,

        // -- SetTapState (scope-DESTRUCTURED, exhaustive over EffectScope): scope:All is
        // the census-ROLE proven exception (TapAll/UntapAll - state-convergent/idempotent,
        // does not escalate over inert growth); scope:Single is an ordinary single announced
        // target. BOTH relax, so both AGREE with effect_target_ctx's scope-blind SetTapState
        // Snapshot arm (the sole dedicated census-role exception, pinned by A7').
        Effect::SetTapState {
            scope: EffectScope::All,
            ..
        } => CensusRole::Relax(RelaxReason::SetTapStateException),
        Effect::SetTapState {
            scope: EffectScope::Single,
            ..
        } => CensusRole::Relax(RelaxReason::BoundedOrNoPopulation),

        // -- Suspect/Unsuspect scope:Single: a single announced target (a2).
        Effect::Suspect {
            scope: EffectScope::Single,
            ..
        }
        | Effect::Unsuspect {
            scope: EffectScope::Single,
            ..
        } => CensusRole::Relax(RelaxReason::BoundedOrNoPopulation),

        // -- ZONE-DISJOINT: reads a non-battlefield pool (library/hand/graveyard/
        // exile/outside-game/stack), disjoint from the battlefield growth class.
        // `target_filter()==None` for most of these, yet correctly RELAXED.
        Effect::Dig { .. }
        | Effect::Seek { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::RevealUntil { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::Surveil { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::Discover { .. }
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::Conjure { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::CollectEvidence { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::CastFromZone { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::RememberCard { .. }
        | Effect::CreateTokenCopyFromPool { .. } => CensusRole::Relax(RelaxReason::ZoneDisjoint),

        // -- BOUNDED / NO BATTLEFIELD POPULATION: a single announced/bounded
        // target, a fixed-category iteration, or a player-/self-only read - none
        // scale with the battlefield growth class.
        Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::RemoveCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::ChangeZone { .. }
        | Effect::GainControl { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::Explore
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Connive { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        // upstream #6070 (Susan Foreman): reorders the PLANAR DECK top (Planechase),
        // not a battlefield population ⇒ not a live-board census (relax).
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ForEachCategory { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => CensusRole::Relax(RelaxReason::BoundedOrNoPopulation),
    }
}

// ---------------------------------------------------------------------------
// Resolution-time choice-freeness classifier (`analysis::resource` item 6).
// A separate question family from the three read-axes above — see the module
// header. Fail-closed default is `MayPrompt`.
// ---------------------------------------------------------------------------

/// CR 732.2a + CR 608.2d: resolution-time choice-freeness verdict for the
/// growing-cascade cover gate (`analysis::resource` item 6). NOT an `Axes`
/// axis — this classifies RESOLVER prompting behavior, not AST reads (module
/// header rationale).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResolutionChoiceFreedom {
    /// Resolving can never enter a non-priority `WaitingFor` in ANY state,
    /// EXCEPT through the life-event replacement pipeline (single optional
    /// candidate, replacement.rs:6221; CR 616.1 material ordering,
    /// replacement.rs:6263; mandatory body-continuation drain,
    /// replacement.rs:5511-5524 → engine_replacement.rs:1159). Callers MUST
    /// pair this verdict with `analysis::resource::life_event_replacements_may_prompt`
    /// — the paired environmental obligation is part of this variant's contract.
    ///
    /// There is deliberately no plain `Free` variant yet: both allow-listed
    /// kinds (`GainLife`/`LoseLife`) genuinely can prompt via the life-event
    /// replacement pipeline, so `Free` would be uninhabited today. Adding it
    /// later is compiler-guided (a new variant flags every exhaustive match).
    FreeUnlessLifeReplacements,
    /// May prompt, or unproven — the fail-closed default.
    MayPrompt,
}

impl ResolutionChoiceFreedom {
    /// Worst-of join for a resolution chain: `MayPrompt` dominates (a chain that
    /// can prompt on either branch can prompt).
    fn join(self, other: ResolutionChoiceFreedom) -> ResolutionChoiceFreedom {
        if matches!(self, ResolutionChoiceFreedom::FreeUnlessLifeReplacements)
            && matches!(other, ResolutionChoiceFreedom::FreeUnlessLifeReplacements)
        {
            ResolutionChoiceFreedom::FreeUnlessLifeReplacements
        } else {
            ResolutionChoiceFreedom::MayPrompt
        }
    }
}

/// CR 608.2d: can resolving this single `Effect` ever offer a resolution-time
/// player choice? Exhaustive `match` with NO wildcard catch-all arm — a NEW
/// `Effect` variant fails to compile here until it is classified. Only the two
/// allow-list arms make a soundness claim (grounded by a resolver trace); every
/// other variant is the fail-closed `MayPrompt` (an ungrounded reject is only a
/// false-negative cover rejection, so grouped arms need no per-kind evidence).
fn effect_resolution_choice_freedom(e: &Effect) -> ResolutionChoiceFreedom {
    match e {
        // ---- allow-list: choice-free EXCEPT the life-event replacement
        //      pipeline (destructured WITHOUT `..` so a new field forces a
        //      re-audit of the soundness claim) ----
        //
        // CR 119.3 + CR 732.2a: resolver trace effects/life.rs — resolve_gain
        // (life.rs:19-110) runs its OWN inline replace_event pipeline; its only
        // prompt is ReplacementResult::NeedsChoice (life.rs:96-101). Player
        // selection = pure filter eval (game/filter.rs: no WaitingFor); amount =
        // pure quantity eval (game/quantity.rs: no WaitingFor). Verdict is
        // payload-independent. CR 119.7 can't-gain short-circuit is deterministic.
        // PAIRED OBLIGATION: caller runs life_event_replacements_may_prompt
        // (resource.rs item 6), which also covers the mandatory body-continuation
        // drain (H4 route c) and the Execute-arm stack.rs drain.
        Effect::GainLife {
            amount: _,
            player: _,
        } => ResolutionChoiceFreedom::FreeUnlessLifeReplacements,
        // CR 119.3 + CR 732.2a: same shape — resolve_lose (life.rs:293-365),
        // only prompt = NeedsChoice (life.rs:352-355). CR 119.8 can't-lose
        // short-circuit is deterministic. Same PAIRED OBLIGATION.
        Effect::LoseLife {
            amount: _,
            target: _,
        } => ResolutionChoiceFreedom::FreeUnlessLifeReplacements,
        // ---- everything else: fail-closed MayPrompt. Grouped so the compiler
        //      still enforces exhaustiveness (every variant is named); no payload
        //      scanning needed on the reject side. ----
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        // CR 701.4a + CR 608.2d: behold may prompt (`WaitingFor::BeholdChoice`
        // when 2+ candidates) — fail-closed MayPrompt.
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => ResolutionChoiceFreedom::MayPrompt,
    }
}

/// CR 732.2a / CR 705.1 / CR 706.1a / CR 701.9b: does resolving this single
/// `Effect` draw on game randomness whose outcome determines the next action — a
/// coin flip (CR 705.1), a die roll (CR 706.1a, incl. the planar / attraction /
/// contraption dice), or a "the game selects uniformly at random" selection
/// (CR 701.9a/b)? A CR 732.2a shortcut "can't include conditional actions, where
/// the outcome of a game event determines the next action," so a loop body
/// bearing any of these is not a legal shortcut. EXHAUSTIVE over `Effect` with NO
/// `_` wildcard — a FUTURE random-bearing variant BUILD-BREAKS here, so it can
/// never be silently offered as deterministic. The false-group is the sibling
/// `effect_resolution_choice_freedom` variant list minus the randomness arms; the
/// compiler enforces that the two lists stay in lockstep. (A2 determinism gate —
/// the static, compile-time-exhaustive half.)
pub(crate) fn effect_is_randomness_bearing(e: &Effect) -> bool {
    match e {
        // --- auto-resolved randomness (no `WaitingFor`; the recast injector cannot
        //     abort on these — they draw the seeded RNG and continue) ---
        Effect::FlipCoin { .. }
        | Effect::FlipCoins { .. }
        | Effect::FlipCoinUntilLose { .. }
        | Effect::RollDie { .. }
        | Effect::ChaosEnsues
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptionsFromRollDifference
        // CR 701.30a: a clash reveals the top card of each player's (shuffled) library — hidden
        // information the recast injector cannot know at pin time. CR 701.30d: the winner is
        // decided by comparing those revealed mana values, so the outcome (and any action it
        // gates) is unpredictable. CR 732.2a bars shortcutting a loop across such a random event,
        // so a recast body containing a clash is randomness-bearing ⇒ fail-closed reject.
        | Effect::Clash => true,
        // --- field-level "game picks at random" (CR 701.9a/b): random ONLY when the
        //     selection mode is `Random`; a `Chosen` selection is a normal player
        //     choice, not randomness. All four `CardSelectionMode` carriers share one
        //     arm; `Choose` (a `TargetSelectionMode`) is a distinct type so it takes
        //     its own arm. `Bounce`/`MoveCounters` carry no `Random` selection mode. ---
        Effect::Discard { selection, .. }
        | Effect::RevealHand { selection, .. }
        | Effect::CreateTokenCopyFromPool { selection, .. }
        | Effect::ChooseFromZone { selection, .. } => selection.is_random(),
        Effect::Choose { selection, .. } => selection.is_random(),
        // --- everything else: NOT randomness. Grouped so the compiler still enforces
        //     exhaustiveness (every variant named; no wildcard). ---
        Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::CounterAll { .. }
        | Effect::Token { .. }
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::OpenAttractions { .. }
        | Effect::AssembleContraptions { .. }
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::Conjure { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => false,
    }
}

/// CR 732.2a: does the recast spell ability (its whole effect tree per CR 608.2,
/// plus its announce-time target selection) bear any randomness? Reuses the
/// exhaustive `ability_graph::collect_effects` walker for traversal, then runs
/// `effect_is_randomness_bearing` over every collected effect. `None`-free /
/// fail-open is impossible: the caller treats an undeterminable ability as a
/// no-offer separately. (A2 determinism gate.)
pub(crate) fn spell_ability_bears_randomness(def: &AbilityDefinition) -> bool {
    // CR 700.2b / CR 701.9b: "choose ... at random" at the ability announce layer
    // (`TargetSelectionMode::Random`, e.g. Cult of Skaro) — the walker collects
    // sub-line effects, not the ability-level selection mode, so check it directly.
    if def.target_selection_mode.is_random() {
        return true;
    }
    let mut effects = Vec::new();
    crate::analysis::ability_graph::collect_effects(def, &mut effects);
    effects.iter().any(|&e| effect_is_randomness_bearing(e))
}

/// CR 608.2d + CR 732.2a: does resolving this ability (its whole chain) ever
/// enter a resolution-time player choice? The `ResolvedAbility` destructure is
/// EXHAUSTIVE with no `..` — the read-walk's `resolved_ability_axes` (:116)
/// classifications are deliberately NOT reused (this is a different question:
/// e.g. `optional` is read-free yet choice-bearing). A FUTURE field fails to
/// compile here until classified for the choice question.
pub(crate) fn ability_resolution_choice_freedom(a: &ResolvedAbility) -> ResolutionChoiceFreedom {
    let ResolvedAbility {
        // ---- choice-bearing: folded into the verdict below ----
        effect,
        sub_ability,
        else_ability,
        optional,
        optional_for,
        optional_targeting,
        unless_pay,
        target_chooser,
        target_choice_timing,
        modal,
        mode_abilities,
        repeat_until,
        // ---- choice-free: bound `_` with a one-line justification ----
        condition: _, // resolution branch selector, pure eval (both branches recursed)
        duration: _,  // continuous-effect lifetime, no prompt
        player_scope: _, // iteration fan-out, pure player-filter eval
        starting_with: _, // APNAP start override, no prompt
        repeat_for: _, // "for each" count, pure quantity eval (game/quantity.rs)
        announced_x: _, // CR 601.2b announce-time count, pure quantity eval, no prompt
        multi_target: _, // announce-time variable-count bounds (Resolution case caught by timing)
        target_constraints: _, // announce-time cross-target legality, no resolution prompt
        distribution: _, // CR 601.2d concrete pre-assigned portions (announce-time)
        targets: _,   // concrete announced target refs (already resolved)
        source_id: _, // object id
        source_incarnation: _, // self-transform epoch latch, no resolution-time choice
        trigger_source: _, // exact triggered-source authority, no choice
        trigger_definition_ref: _, // exact trigger occurrence, no choice
        controller: _, // player id
        original_controller: _, // player id
        scoped_player: _, // player id (iteration binding)
        kind: _,      // AbilityKind tag (no payload)
        context: _,   // SpellContext: cast-time fact snapshot, not a live choice
        description: _, // display string
        selected_mode_labels: _, // display strings, no resolution-time choice
        min_x_value: _, // u32
        cant_be_copied: _, // bool
        copy_count_status: _, // status tag
        forward_result: _, // bool
        chosen_x: _,  // concrete cast-time X (chosen at announcement, not resolution)
        cost_paid_object: _, // concrete captured-object snapshot
        cost_paid_object_ids: _, // concrete captured-object ids (issue #4948)
        effect_context_object: _, // concrete captured-object snapshot
        amassed_army_object: _, // concrete captured-object snapshot
        ability_index: _, // usize provenance
        may_trigger_origin: _, // provenance tag
        target_selection_mode: _, // Chosen/Random tag (announce-time)
        chosen_players: _, // concrete chosen player ids (already selected)
        replacement_applied: _, // replacement provenance set, no prompt
        sub_link: _,  // SubAbilityLink kind tag
        parent_target_missing_reason: _, // seam flag
    } = a;

    // CR 608.2d: an optional effect / optional targeting / opponent-may
    // effect prompts the controller (or opponent) before execution
    // (WaitingFor::OptionalEffectChoice, effects/mod.rs:4294).
    if *optional || *optional_targeting || optional_for.is_some() {
        return ResolutionChoiceFreedom::MayPrompt;
    }
    // CR 118.12: "unless a player pays {cost}" is a resolution-time pay prompt
    // (also item-4 redundant — ability_scan.rs sets `projected` for it).
    if unless_pay.is_some() {
        return ResolutionChoiceFreedom::MayPrompt;
    }
    // CR 601.2c + CR 603.3d: a resolution-time target chooser announces targets (H3).
    if target_chooser.is_some() {
        return ResolutionChoiceFreedom::MayPrompt;
    }
    // CR 608.2d: resolution-timed target selection is a resolution-time choice even
    // though `targets` is empty on the stack, which the ordering gate can't see (H3).
    if matches!(target_choice_timing, TargetChoiceTiming::Resolution) {
        return ResolutionChoiceFreedom::MayPrompt;
    }
    // CR 700.2b + CR 603.3c: a modal header / reflexive per-mode abilities open a
    // mode choice at resolution (conservative — rejected even when the mode is baked).
    if modal.is_some() || !mode_abilities.is_empty() {
        return ResolutionChoiceFreedom::MayPrompt;
    }
    // CR 608.2c + CR 107.1c: only the controller-prompted repeat variant is a
    // player choice; while / until-stop predicates are pure re-evaluation.
    if matches!(repeat_until, Some(RepeatContinuation::ControllerChoice)) {
        return ResolutionChoiceFreedom::MayPrompt;
    }

    // CR 608.2c: the chain resolves the effect and, on the taken branch, a
    // sub_ability / else_ability effect — join both (fail-safe: reject if either
    // can prompt).
    let mut acc = effect_resolution_choice_freedom(effect);
    if let Some(sub) = sub_ability {
        acc = acc.join(ability_resolution_choice_freedom(sub));
    }
    if let Some(else_branch) = else_ability {
        acc = acc.join(ability_resolution_choice_freedom(else_branch));
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AggregateFunction, CastManaObjectScope, CastManaSpentMetric, Comparator, ManaContribution,
        StaticDefinition,
    };
    use crate::types::counter::CounterType;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::ManaColor;
    use crate::types::player::{PlayerCounterKind, PlayerId};

    fn ability_with_amount(qty: QuantityRef) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref { qty },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn fixed_drain() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
    }

    // ---- P0/P2: the ScanMode split + descending object-growth firewall ----

    /// A read-free vanilla token (Presence of Gond's "1/1 green Elf Warrior"):
    /// fixed P/T, no keywords, fixed count, controller owner, no statics/counters.
    fn vanilla_token() -> Effect {
        Effect::Token {
            name: "Elf Warrior".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![ManaColor::Green],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        }
    }

    /// A board `ObjectCount` — the canonical sibling-mutable dynamic quantity
    /// (`scan_quantity_ref::ObjectCount` self-asserts `sibling`).
    fn object_count() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            },
        }
    }

    /// P0-1: a vanilla token stays fail-closed CONSERVATIVE in `Conservative` mode.
    /// Revert-probe: make the Token arm descend unconditionally ⇒ `event` flips false.
    #[test]
    fn conservative_mode_token_axes_are_unchanged() {
        let axes = scan_effect(&vanilla_token(), ScanMode::Conservative);
        assert!(axes.event && axes.sibling && axes.projected);
    }

    /// P0-2: same for `Effect::Mana`.
    /// Revert-probe: make the Mana arm descend unconditionally ⇒ `event` flips false.
    #[test]
    fn conservative_mode_mana_axes_are_unchanged() {
        let mana = Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };
        let axes = scan_effect(&mana, ScanMode::Conservative);
        assert!(axes.event && axes.sibling && axes.projected);
    }

    /// P0-3: the CR 603.3b trigger-ordering gate is byte-identical for a token-bodied
    /// trigger — it stays order-DEPENDENT (prompts). Uses the PUBLIC entries that
    /// `game::triggers` consumes (which pass `Conservative`). Revert-probe: descend
    /// the shared arm in `Conservative` ⇒ event/sibling drop ⇒ `c2` flips to true
    /// (spurious auto-order).
    #[test]
    fn cr_603_3b_gate_is_byte_identical_for_a_token_trigger() {
        let ability = ResolvedAbility::new(vanilla_token(), Vec::new(), ObjectId(1), PlayerId(0));
        let c2 = !ability_uses_event_context(&ability) && !ability_reads_sibling_mutable(&ability);
        assert!(
            !c2,
            "token-bodied trigger must stay order-dependent (CR 603.3b)"
        );
    }

    /// P0-4: the same vanilla token DESCENDS to NONE in `LoopFirewall` (reads
    /// nothing); a dynamic-count token descends to a sibling read. The vanilla→NONE
    /// control proves the sibling in the dynamic case is carried by `count` alone.
    /// Revert-probe: bind `count` to `_` in the Token arm ⇒ the dynamic assertion flips.
    #[test]
    fn loop_firewall_mode_token_axes_descend() {
        let axes = scan_effect(&vanilla_token(), ScanMode::LoopFirewall);
        assert!(!axes.event && !axes.sibling && !axes.projected);

        let mut dyn_tok = vanilla_token();
        if let Effect::Token { count, .. } = &mut dyn_tok {
            *count = object_count();
        }
        assert!(scan_effect(&dyn_tok, ScanMode::LoopFirewall).sibling);
    }

    /// P2-1: a fixed anthem modification reads nothing (control for P2-2).
    #[test]
    fn fixed_anthem_modification_reads_nothing() {
        let axes = scan_continuous_modification(
            &ContinuousModification::AddPower { value: 2 },
            ScanMode::LoopFirewall,
        );
        assert!(!axes.event && !axes.sibling && !axes.projected);
    }

    /// P2-2: a dynamic-P/T modification reads a sibling aggregate.
    /// Revert-probe: move the arm into the read-free bucket (⇒ NONE) ⇒ fails.
    #[test]
    fn dynamic_pt_modification_reads_sibling() {
        let m = ContinuousModification::SetDynamicPower {
            value: object_count(),
        };
        assert!(scan_continuous_modification(&m, ScanMode::LoopFirewall).sibling);
    }

    /// P2-3: a token whose `enter_with_counters` count is a board `ObjectCount`
    /// reads sibling. The vanilla control (P0-4) has empty counters ⇒ NONE, so the
    /// sibling is carried by `enter_with_counters` alone. Revert-probe: bind
    /// `enter_with_counters` to `_` in the Token arm ⇒ flips.
    #[test]
    fn token_effect_with_dynamic_enter_counters_reads_sibling() {
        let mut tok = vanilla_token();
        if let Effect::Token {
            enter_with_counters,
            ..
        } = &mut tok
        {
            *enter_with_counters = vec![(CounterType::Plus1Plus1, object_count())];
        }
        assert!(scan_effect(&tok, ScanMode::LoopFirewall).sibling);
    }

    /// P2-4: a token whose granted static ability carries a dynamic-P/T modification
    /// reads sibling. Revert-probe: bind `static_abilities` to `_` in the Token arm
    /// ⇒ flips.
    #[test]
    fn token_effect_with_dynamic_static_ability_reads_sibling() {
        let mut tok = vanilla_token();
        if let Effect::Token {
            static_abilities, ..
        } = &mut tok
        {
            *static_abilities = vec![StaticDefinition::continuous().modifications(vec![
                ContinuousModification::SetDynamicPower {
                    value: object_count(),
                },
            ])];
        }
        assert!(scan_effect(&tok, ScanMode::LoopFirewall).sibling);
    }

    /// P2-5 (B4): a token carrying a growing-cost keyword (Convoke — a UNIT variant
    /// that reads the board) reads sibling. Proves payload-SHAPE classification is
    /// insufficient: `keyword_cost_reads_growing_class` is the semantic authority.
    /// Revert-probe: bind `keywords` to `_` in the Token arm ⇒ flips.
    #[test]
    fn token_effect_with_growing_cost_keyword_reads_sibling() {
        let mut tok = vanilla_token();
        if let Effect::Token { keywords, .. } = &mut tok {
            *keywords = vec![Keyword::Convoke];
        }
        assert!(scan_effect(&tok, ScanMode::LoopFirewall).sibling);
    }

    /// P2-6 (B5): `AddCounterOnEnter` with a dynamic count reads sibling — it looks
    /// structural but carries a `QuantityExpr`. Revert-probe: sweep the arm into the
    /// read-free bucket ⇒ flips.
    #[test]
    fn add_counter_on_enter_modification_reads_sibling() {
        let m = ContinuousModification::AddCounterOnEnter {
            counter_type: CounterType::Plus1Plus1,
            count: object_count(),
            if_type: None,
        };
        assert!(scan_continuous_modification(&m, ScanMode::LoopFirewall).sibling);
    }

    /// P2-7 (B2 + R1): a board-color aggregate self-asserts its OWN `sibling`, even
    /// with a NON-`Typed` filter (`Controller` ⇒ `scan_target_filter` = NONE), so
    /// the signal cannot come from the `Typed` arm. Revert-probe: strip the arm's
    /// own `sibling:true` literal (delegate to `scan_target_filter` only) ⇒ with a
    /// non-`Typed` filter, flips to false.
    #[test]
    fn mana_board_aggregate_self_asserts_sibling() {
        let p = ManaProduction::DistinctColorsAmongPermanents {
            filter: TargetFilter::Controller,
        };
        assert!(scan_mana_production(&p, ScanMode::LoopFirewall).sibling);
    }

    /// P2-8 (B2): `TriggerEventManaType` reads the triggering event (event axis).
    /// Revert-probe: bin it NONE ⇒ flips.
    #[test]
    fn mana_production_trigger_event_type_is_conservative() {
        assert!(
            scan_mana_production(
                &ManaProduction::TriggerEventManaType,
                ScanMode::LoopFirewall
            )
            .event
        );
    }

    /// P2-9: Gaea's Cradle's `{T}: Add {G} for each creature you control` is the
    /// MEASURED shape `AnyOneColor{count: Ref(ObjectCount{Typed{Creature}}), ...}` —
    /// a COUNT-path arm whose sibling comes from `count` → `scan_quantity_ref::
    /// ObjectCount` (NOT a board-aggregate literal, distinct from P2-7's FILTER
    /// path). Revert-probe: bind the mana arm's `count` to `_` ⇒ flips to NONE.
    #[test]
    fn gaeas_cradle_count_path_vetoes() {
        let p = ManaProduction::AnyOneColor {
            count: object_count(),
            color_options: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        };
        assert!(scan_mana_production(&p, ScanMode::LoopFirewall).sibling);
    }

    // ---- P3 (DEFERRED-8): CR 732.2a Typed-precision census discipline ----

    /// A5: the census-discipline structural invariant. Under `LoopFirewall`,
    /// `SnapshotOrEvent` + a bare `Typed` (and its Not/Or/And wrappers) relaxes to
    /// `sibling:false`; a board-reading property keeps it true (fail-closed); and
    /// `LiveBoardCensus` yields `sibling:true` for ANY filter shape (the census base,
    /// also fixing the latent non-`Typed` board-filter miss, "bug (a)").
    #[test]
    fn snapshot_ctx_yields_no_sibling_under_loopfirewall() {
        use crate::types::ability::FilterProp;
        use FilterReadContext::{LiveBoardCensus, SnapshotOrEvent};
        use ScanMode::LoopFirewall;
        let bare = TargetFilter::Typed(TypedFilter::creature());
        assert!(!scan_target_filter(&bare, SnapshotOrEvent, LoopFirewall).sibling);
        let notf = TargetFilter::Not {
            filter: Box::new(bare.clone()),
        };
        assert!(!scan_target_filter(&notf, SnapshotOrEvent, LoopFirewall).sibling);
        let orf = TargetFilter::Or {
            filters: vec![bare.clone()],
        };
        assert!(!scan_target_filter(&orf, SnapshotOrEvent, LoopFirewall).sibling);
        let andf = TargetFilter::And {
            filters: vec![bare.clone()],
        };
        assert!(!scan_target_filter(&andf, SnapshotOrEvent, LoopFirewall).sibling);
        // A board-reading property (nested `Targets{Typed}`) keeps sibling:true even
        // under SnapshotOrEvent (a journal/board-reading prop still vetoes).
        let prop_typed =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Targets {
                    filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
                }]),
            );
        assert!(scan_target_filter(&prop_typed, SnapshotOrEvent, LoopFirewall).sibling);
        // LiveBoardCensus ⇒ sibling:true for ANY shape (census base / bug-(a) fix).
        assert!(scan_target_filter(&bare, LiveBoardCensus, LoopFirewall).sibling);
        assert!(
            scan_target_filter(&TargetFilter::Controller, LiveBoardCensus, LoopFirewall).sibling
        );
        assert!(scan_target_filter(&TargetFilter::Any, LiveBoardCensus, LoopFirewall).sibling);
    }

    /// A6 (REQ-2): each `LiveBoardCensus` HOLE arm (the R1/G5 defect set, incl. GAP-1
    /// `ControllerControlsMatching` and GAP-2 `ZoneCardCount`) yields `sibling:true`
    /// under LoopFirewall with a bare `Typed{Creature}` — the census base, NOT the
    /// (relaxed) `Typed` arm, carries the veto. The control proves it is load-bearing:
    /// the SAME bare `Typed` under `SnapshotOrEvent` relaxes to `sibling:false`, so
    /// flipping any arm's ctx to `SnapshotOrEvent` would relax it into a false
    /// certificate (the executed flip→FAIL revert-probe is documented in the report).
    #[test]
    fn census_hole_arms_are_load_bearing() {
        use crate::types::ability::{
            AbilityCondition, CountScope, QuantityRef, ReplacementCondition, SharedQuality,
            StaticCondition, TriggerCondition, ZoneRef,
        };
        use FilterReadContext::SnapshotOrEvent;
        use ScanMode::LoopFirewall;
        let ct = || TargetFilter::Typed(TypedFilter::creature());
        // Control: SnapshotOrEvent relaxes this exact filter (non-vacuity anchor).
        assert!(!scan_target_filter(&ct(), SnapshotOrEvent, LoopFirewall).sibling);

        assert!(
            scan_static_condition(
                &StaticCondition::IsPresent { filter: Some(ct()) },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_static_condition(
                &StaticCondition::DefendingPlayerControls { filter: ct() },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_trigger_condition(
                &TriggerCondition::MinCoAttackers {
                    minimum: 1,
                    filter: Some(ct())
                },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_trigger_condition(
                &TriggerCondition::ControlsNone { filter: ct() },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_trigger_condition(
                &TriggerCondition::DefendingPlayerControlsNone { filter: ct() },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_replacement_condition(
                &ReplacementCondition::UnlessControlsMatching { filter: ct() },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_replacement_condition(
                &ReplacementCondition::UnlessControlsCountMatching {
                    minimum: 1,
                    filter: ct()
                },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_replacement_condition(
                &ReplacementCondition::IfControlsMatching {
                    minimum: 1,
                    filter: ct()
                },
                LoopFirewall
            )
            .sibling
        );
        // GAP-1: ControllerControlsMatching (live board census, effects/mod.rs:9492).
        assert!(
            scan_ability_condition(
                &AbilityCondition::ControllerControlsMatching { filter: ct() },
                LoopFirewall
            )
            .sibling
        );
        // Dual-read ObjectsShareQuality (subject + reference both census).
        assert!(
            scan_ability_condition(
                &AbilityCondition::ObjectsShareQuality {
                    subject: ct(),
                    reference: ct(),
                    quality: SharedQuality::Name,
                },
                LoopFirewall
            )
            .sibling
        );
        // GAP-2: ZoneCardCount (battlefield-scoped census, unconditional fail-closed).
        assert!(
            scan_quantity_ref(
                &QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types: vec![],
                    filter: Some(ct()),
                    scope: CountScope::Controller,
                },
                LoopFirewall
            )
            .sibling
        );
        assert!(
            scan_quantity_ref(
                &QuantityRef::FilteredTrackedSetSize {
                    filter: Box::new(ct()),
                    caused_by: None,
                },
                LoopFirewall
            )
            .sibling
        );
    }

    /// guard#3 (mitigation #3): the `LiveBoardCensus` tag set of `effect_target_ctx`
    /// == EXACTLY the enumeration-derived MASS-POPULATION set (28). Source-scanned, not
    /// hand-counted (the hand-count is what produced the earlier "relax=4" miss). Under
    /// B's SnapshotOrEvent default this is the primary false-certificate gate: only a
    /// census tag vetoes a mass read that ESCALATES over inert token growth (which
    /// `grown_objects_are_inert` cannot catch — obligation-(ii) is never a relax
    /// license for a census read). A dropped tag (a mass slot silently on the Snapshot
    /// default) OR an added one turns this RED, forcing a conscious re-audit.
    #[test]
    fn census_tag_set_is_exactly_enumerated() {
        let src = include_str!("ability_scan.rs");
        let start = src.find("fn effect_target_ctx(").expect("fn");
        let fnsrc = &src[start..start + src[start..].find("\n// ----").expect("divider")];
        let arm_end = fnsrc
            .find("=> FilterReadContext::LiveBoardCensus,")
            .expect("census arm");
        let arm_start = fnsrc[..arm_end]
            .rfind("GENUINELY-CENSUS")
            .expect("census comment");
        let block = &fnsrc[arm_start..arm_end];
        let mut got: Vec<&str> = block
            .match_indices("Effect::")
            .map(|(i, _)| {
                let s = &block[i + "Effect::".len()..];
                let e = s
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(s.len());
                &s[..e]
            })
            .collect();
        got.sort_unstable();
        got.dedup();
        let mut want = [
            "BounceAll",
            "ChangeZoneAll",
            "ChooseAndSacrificeRest",
            "ChooseObjectsIntoTrackedSet",
            "CounterAll",
            "DamageAll",
            "DamageEachPlayer",
            "DestroyAll",
            "DoublePTAll",
            "EachDealsDamageEqualToPower",
            "EachPlayerCopyChosen",
            "EachSourceDealsDamage",
            "ExploreAll",
            "GainControlAll",
            "GoadAll",
            "PumpAll",
            "PutCounterAll",
            // R1: Suspect/Unsuspect scope:All are mass-population battlefield reads
            // (`suspect.rs` enumerates `state.battlefield`, `target_filter()`==None).
            // Their `Effect::` name appears in the census `|`-chain scope-gated on
            // `EffectScope::All`; the scope:Single arms live in the relax group below and
            // are NOT scanned here (they sit past the census terminator).
            "Suspect",
            "UnattachAll",
            "Unsuspect",
            // P3-B round-2: F1-class dual-mode mass-battlefield resolvers (a resolver
            // mode enumerates the battlefield and applies to EVERY matching object when
            // no explicit object target is chosen; no static discriminator ⇒ whole
            // variant censuses, fail-closed). See the census-arm comment for CR cites.
            "BecomeCopy",
            "CopyTokenBlockingAttacker",
            "GainActivatedAbilitiesOfTarget",
            "MultiplyCounter",
            "PhaseIn",
            "PhaseOut",
            "TurnFaceDown",
            "TurnFaceUp",
        ];
        want.sort_unstable();
        assert_eq!(
            got, want,
            "census tag set drifted from the enumeration-derived mass-population set"
        );
        assert_eq!(got.len(), 28, "exactly 28 mass-population census tags");
    }

    /// A7' (mitigation #4, replaces the void census-default A7): with SnapshotOrEvent the
    /// DEFAULT (author's contract restored), the obligation-(ii)-PROVEN census-role
    /// exception set == EXACTLY {SetTapState}. SetTapState is census-ROLE ("tap/untap
    /// all", scope All) yet relaxes because tap-state is state-convergent/idempotent
    /// (a specific proven non-escalation, NOT a general (b)-license). Structurally it is
    /// the SOLE effect with a DEDICATED SnapshotOrEvent arm (the region between the
    /// census arm and the single-object group); giving any OTHER census-role slot a
    /// dedicated Snapshot arm turns this RED. Dual-guard with
    /// `census_tag_set_is_exactly_enumerated` (guard#3, pins the 28 census tags).
    #[test]
    fn obligation_ii_census_exception_is_exactly_settapstate() {
        use crate::types::ability::{EffectScope, TapStateChange};
        use ScanMode::{Conservative, LoopFirewall};
        // Behavioral: the census-role SetTapState relaxes under LoopFirewall and stays
        // byte-identical (SnapshotOrEvent) under Conservative.
        let settap = Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::All,
            state: TapStateChange::Untap,
        };
        assert_eq!(
            effect_target_ctx(&settap, LoopFirewall),
            FilterReadContext::SnapshotOrEvent
        );
        assert_eq!(
            effect_target_ctx(&settap, Conservative),
            FilterReadContext::SnapshotOrEvent
        );
        // Structural: SetTapState is the ONLY dedicated-arm Snapshot classification.
        let src = include_str!("ability_scan.rs");
        let start = src.find("fn effect_target_ctx(").expect("fn");
        let fnsrc = &src[start..start + src[start..].find("\n// ----").expect("divider")];
        let after_census = &fnsrc[fnsrc
            .find("=> FilterReadContext::LiveBoardCensus,")
            .expect("census terminator")..];
        let dedicated = &after_census[.."=> FilterReadContext::LiveBoardCensus,".len()
            + after_census["=> FilterReadContext::LiveBoardCensus,".len()..]
                .find("=> FilterReadContext::SnapshotOrEvent,")
                .expect("first snapshot terminator")];
        let names: Vec<&str> = dedicated
            .match_indices("Effect::")
            .map(|(i, _)| {
                let s = &dedicated[i + "Effect::".len()..];
                let e = s
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(s.len());
                &s[..e]
            })
            .collect();
        assert_eq!(
            names,
            vec!["SetTapState"],
            "the sole dedicated-Snapshot (census-role exception) arm must be SetTapState"
        );
    }

    /// R1 (CR 701.60a): Suspect/Unsuspect census classification is SCOPE-SENSITIVE,
    /// mirroring `target_filter()` (Some for scope:Single, None for scope:All).
    /// scope:All is a mass battlefield population read (`suspect.rs` enumerates
    /// `state.battlefield`) => `LiveBoardCensus`; scope:Single is a single announced
    /// target => `SnapshotOrEvent`. DISCRIMINATING: reverting the scope:All arm back
    /// into the relax group flips the `LiveBoardCensus` assertions to `SnapshotOrEvent`
    /// (a false-certificate relax), turning this RED.
    #[test]
    fn suspect_unsuspect_census_is_scope_sensitive() {
        use crate::types::ability::EffectScope;
        use ScanMode::LoopFirewall;
        let f = || TargetFilter::Typed(TypedFilter::creature());
        let cases = [
            (
                Effect::Suspect {
                    target: f(),
                    scope: EffectScope::All,
                },
                Effect::Suspect {
                    target: f(),
                    scope: EffectScope::Single,
                },
            ),
            (
                Effect::Unsuspect {
                    target: f(),
                    scope: EffectScope::All,
                },
                Effect::Unsuspect {
                    target: f(),
                    scope: EffectScope::Single,
                },
            ),
        ];
        for (all, single) in cases {
            assert_eq!(
                effect_target_ctx(&all, LoopFirewall),
                FilterReadContext::LiveBoardCensus,
                "scope:All is a mass battlefield read => census (fail-closed)"
            );
            assert_eq!(
                effect_target_ctx(&single, LoopFirewall),
                FilterReadContext::SnapshotOrEvent,
                "scope:Single is a single announced target => relax"
            );
        }
    }

    /// F3 (CR 732.2a): the independent census PARTITION (`effect_census_role`) agrees
    /// with `effect_target_ctx` on the Census/Relax boundary, closing the F1 gap where a
    /// census-ROLE slot silently in the generic relax `|`-chain (exactly R1's Suspect{All})
    /// is invisible to the census-arm-only guards. Structural: both functions' `Census`
    /// name-sets are source-scanned and asserted IDENTICAL (== the 28). Behavioral: the
    /// two oracles agree on every discriminator, incl. BOTH Suspect/Unsuspect scopes.
    ///
    /// REVERT-PROBE (discrimination proof): moving `Suspect{All}` out of the census arm of
    /// EITHER function breaks this guard — if only `effect_target_ctx` is reverted the
    /// source-scanned census sets diverge (structural `assert_eq!` fails); if
    /// `effect_census_role` misclassifies it as `Relax` the behavioral `Census` assertion
    /// flips. Executed: reclassifying Suspect{All} to `Relax(BoundedOrNoPopulation)` in
    /// `effect_census_role` made this test FAIL on both the set-equality and the behavioral
    /// `census(&Suspect{All}, true)` assertion, then was restored.
    #[test]
    fn census_partition_agrees_with_effect_target_ctx() {
        use crate::types::ability::{EffectScope, TapStateChange};
        use ScanMode::LoopFirewall;

        // -- Structural: the two census name-sets are byte-identical (and == 28).
        fn census_names(fnsrc: &str, terminator: &str) -> Vec<String> {
            let end = fnsrc.find(terminator).expect("census terminator");
            let block = &fnsrc[..end];
            let mut v: Vec<String> = block
                .match_indices("Effect::")
                .map(|(i, _)| {
                    let s = &block[i + "Effect::".len()..];
                    let e = s
                        .find(|c: char| !c.is_alphanumeric() && c != '_')
                        .unwrap_or(s.len());
                    s[..e].to_string()
                })
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        }
        let src = include_str!("ability_scan.rs");
        let etc_start = src.find("fn effect_target_ctx(").expect("etc");
        let etc = &src[etc_start..etc_start + src[etc_start..].find("\n// ----").expect("etc div")];
        let ecr_start = src.find("fn effect_census_role(").expect("ecr");
        let ecr = &src[ecr_start..ecr_start + src[ecr_start..].find("\n}\n").expect("ecr end")];
        let etc_census = census_names(etc, "=> FilterReadContext::LiveBoardCensus,");
        let ecr_census = census_names(ecr, "=> CensusRole::Census,");
        assert_eq!(
            etc_census, ecr_census,
            "effect_census_role Census set diverged from effect_target_ctx"
        );
        assert_eq!(ecr_census.len(), 28, "exactly 28 census members");

        // -- Behavioral: the two oracles agree on the Census/Relax boundary for every
        // discriminator. `census(e, true)` requires BOTH `effect_census_role == Census`
        // AND `effect_target_ctx == LiveBoardCensus`; `census(e, false)` requires both to
        // be the relax verdict, so neither oracle can drift alone.
        let f = || TargetFilter::Typed(TypedFilter::creature());
        let census = |e: &Effect, want: bool| {
            assert_eq!(
                effect_census_role(e) == CensusRole::Census,
                want,
                "effect_census_role census mismatch: {e:?}"
            );
            assert_eq!(
                effect_target_ctx(e, LoopFirewall) == FilterReadContext::LiveBoardCensus,
                want,
                "effect_target_ctx census mismatch: {e:?}"
            );
        };
        census(
            &Effect::Suspect {
                target: f(),
                scope: EffectScope::All,
            },
            true,
        );
        census(
            &Effect::Unsuspect {
                target: f(),
                scope: EffectScope::All,
            },
            true,
        );
        census(
            &Effect::Suspect {
                target: f(),
                scope: EffectScope::Single,
            },
            false,
        );
        census(
            &Effect::Unsuspect {
                target: f(),
                scope: EffectScope::Single,
            },
            false,
        );
        let settap = Effect::SetTapState {
            target: f(),
            scope: EffectScope::All,
            state: TapStateChange::Untap,
        };
        census(&settap, false);
        census(&Effect::HeistExile, false);
        census(&Effect::NoOp, false);

        // -- Reason sub-tags reachable and correct (documentation-grade, unenforced by the
        // Census/Relax boundary but proving each `RelaxReason` arm is live).
        assert_eq!(
            effect_census_role(&settap),
            CensusRole::Relax(RelaxReason::SetTapStateException)
        );
        assert_eq!(
            effect_census_role(&Effect::HeistExile),
            CensusRole::Relax(RelaxReason::ZoneDisjoint)
        );
        assert_eq!(
            effect_census_role(&Effect::NoOp),
            CensusRole::Relax(RelaxReason::BoundedOrNoPopulation)
        );

        // Invariant-1 proof: SetTapState is scope-DESTRUCTURED in effect_census_role.
        // scope:Single is an ordinary single target (BoundedOrNoPopulation), scope:All is
        // the SetTapStateException; BOTH relax and BOTH agree with effect_target_ctx.
        let settap_single = Effect::SetTapState {
            target: f(),
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        };
        census(&settap_single, false);
        assert_eq!(
            effect_census_role(&settap_single),
            CensusRole::Relax(RelaxReason::BoundedOrNoPopulation),
            "SetTapState{{Single}} must classify by scope, not scope-blind"
        );

        // Invariant-3 proof: the canonical zone-disjoint reads (library/hand pools,
        // disjoint from the battlefield growth class) are RELAX in BOTH oracles - they must
        // NOT appear in either Census set. `target_filter()==None` for these, so a naive
        // "target_filter()==None => census" rule would wrongly fail-CLOSED on them; both
        // census-role oracles correctly relax and AGREE.
        for zd in ["Dig", "Seek", "SearchOutsideGame", "RevealHand"] {
            assert!(
                !etc_census.iter().any(|n| n == zd),
                "{zd} must be RELAX in effect_target_ctx (zone-disjoint)"
            );
            assert!(
                !ecr_census.iter().any(|n| n == zd),
                "{zd} must be RELAX in effect_census_role (zone-disjoint)"
            );
        }
    }

    /// P3-B round-2 (CR 732.2a): the six team-ruled + two audit-found F1-class mass-
    /// battlefield resolvers each census in BOTH oracles under `LoopFirewall`. Each
    /// enumerates the battlefield and applies the effect to EVERY matching object (scales
    /// with the growing class) — six via a dual-mode "no explicit target ⇒ mass scan"
    /// fallback, `CopyTokenBlockingAttacker` UNCONDITIONALLY — so relaxing its filter read
    /// risks a false combo certificate. Per the team-lead whole-variant / fail-closed
    /// ruling there is no static discriminator between announced-single and mass modes, so
    /// the entire variant censuses.
    ///
    /// DISCRIMINATING (revert-probe): moving ANY one of these eight back into the
    /// `SnapshotOrEvent` relax arm of `effect_target_ctx` (or the `Relax(_)` arm of
    /// `effect_census_role`) flips its assertion below to a mismatch, turning this RED.
    /// Executed for each member (e.g. `PhaseOut`, `MultiplyCounter`,
    /// `CopyTokenBlockingAttacker`) — each reverted tag made exactly one `assert_eq!`
    /// iteration FAIL, then was restored.
    #[test]
    fn round2_mass_battlefield_resolvers_census_in_both_oracles() {
        use crate::types::ability::GrantedAbilityScope;
        use ScanMode::LoopFirewall;
        let f = || TargetFilter::Typed(TypedFilter::creature());
        // One instance per new census variant. The payload is irrelevant to the verdict
        // (both oracles match on the variant, not the fields) — the point is the variant.
        let cases: Vec<Effect> = vec![
            Effect::PhaseOut { target: f() },
            Effect::PhaseIn { target: f() },
            Effect::GainActivatedAbilitiesOfTarget {
                target: f(),
                recipient: f(),
                scope: GrantedAbilityScope::ActivatedOnly,
                duration: None,
            },
            Effect::BecomeCopy {
                target: f(),
                recipient: f(),
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![],
            },
            Effect::TurnFaceUp { target: f() },
            Effect::TurnFaceDown {
                target: f(),
                profile: None,
            },
            Effect::MultiplyCounter {
                counter_type: CounterType::Plus1Plus1,
                multiplier: 2,
                target: f(),
            },
            Effect::CopyTokenBlockingAttacker {
                source_filter: f(),
                owner: TargetFilter::Controller,
            },
        ];
        assert_eq!(cases.len(), 8, "the eight round-2 census additions");
        for e in &cases {
            assert_eq!(
                effect_target_ctx(e, LoopFirewall),
                FilterReadContext::LiveBoardCensus,
                "effect_target_ctx must census this mass read (fail-closed): {e:?}"
            );
            assert_eq!(
                effect_census_role(e),
                CensusRole::Census,
                "effect_census_role must census: {e:?}"
            );
        }
    }

    /// P3-B round-2 (CR 732.2a): DURABLE forward-protection for the F1 silent-miss class
    /// at the RESOLVER layer. Scans every `game/effects/*.rs` source for the (broadened)
    /// MASS-BATTLEFIELD-SCAN idiom and asserts the matching file set == a curated
    /// classification. A NEW resolver file that adds the idiom (or a curated file that
    /// stops matching) fails this test until a human re-classifies — the resolver-level
    /// analogue of the census oracles' no-wildcard forcing. This is a HEURISTIC
    /// defense-in-depth guard, NOT a proof; the manual resolver audit + the exhaustive
    /// `effect_census_role` oracle are the completeness authority.
    ///
    /// Idiom = union of two signals (team-lead round-2 ruling — RECALL over precision; the
    /// earlier `from_targets`-gated key PROVABLY missed the guard-varied mass reads, so the
    /// guard gating is dropped entirely from the detector):
    ///   (a) a reference to `resolved_battlefield_object_ids(` — the shared dual-mode
    ///       helper (`mod.rs` defines it; `turn_face_up`/`turn_face_down` delegate with
    ///       zero inline scan, so this call-substring is load-bearing, not optional);
    ///   (b) a battlefield-population enumeration (`battlefield_phased_in_ids` /
    ///       `zone_object_ids`) filtered by `matches_target_filter`, REGARDLESS of guard.
    /// A false positive costs one Relax entry; a missed mass read is a soundness gap — so
    /// the flood is CLASSIFIED, never re-narrowed with an allowlist. Note the ORACLE
    /// (exhaustive `effect_census_role`) is the PRIMARY completeness guarantee; this test
    /// is forward-protection only.
    ///
    /// KNOWN BLIND SPOT & CONSIDERED-AND-EXCLUDED (non-idiom battlefield readers).
    /// Signal (b) keys on the helper enumerators `battlefield_phased_in_ids` /
    /// `zone_object_ids`, so a resolver iterating via raw `state.battlefield.iter()` /
    /// `.values()` — WITH OR WITHOUT `matches_target_filter` — is NOT flagged here:
    ///   - MASS raw-iter reads (`GainActivatedAbilitiesOfTarget`, `BecomeCopy`) are
    ///     unflagged by this test but ARE census in the exhaustive `effect_census_role`
    ///     oracle (the completeness authority), so soundness holds. This test guards
    ///     helper-enumerator mass reads on existing relaxed variants; raw-iteration mass
    ///     reads rely on the oracle's no-wildcard forcing.
    ///   - BOUNDED raw-iter / O(1) reads are deliberately kept OUT of `CLASSIFIED` (so the
    ///     set-equality stays over the 14 idiom-matched files — no allowlist pollution):
    ///     `vote.rs` (`votes_per_session_for` = 1 + count of `GrantsExtraVote` statics,
    ///     snapshotted at session start — bounded single outcome) and `switch_pt.rs`
    ///     (O(1) `state.battlefield.contains()` over the effect's own `ids` — bounded
    ///     SelfRef). Their `Vote` / `SwitchPT` variants correctly RELAX in the oracle.
    ///
    /// NON-VACUITY: (i) deleting/adding any file in `CLASSIFIED` makes `matched != curated`
    /// (the set-equality `assert_eq!` fails); (ii) reverting a census-tag drops the
    /// variant from the source-scanned `effect_census_role` census set, so the census-tie
    /// `assert!` fails. Both were executed and observed RED, then restored.
    #[test]
    fn dual_mode_mass_battlefield_resolvers_are_classified() {
        use std::collections::BTreeSet;

        fn matches_idiom(src: &str) -> bool {
            let signal_a = src.contains("resolved_battlefield_object_ids(");
            let signal_b = (src.contains("battlefield_phased_in_ids")
                || src.contains("zone_object_ids"))
                && src.contains("matches_target_filter");
            signal_a || signal_b
        }

        // Curated classification of EVERY file matching the broadened idiom:
        // (file, is_census, reason). Census = holds a mass battlefield resolver whose read
        // scales with the growing class. Relax = bounded selection / bounded aggregate /
        // zone-disjoint pool / vetoed by a different mechanism (scan_effect CONSERVATIVE).
        const CLASSIFIED: &[(&str, bool, &str)] = &[
            // ---- CENSUS (mass battlefield resolver present) ----
            (
                "change_zone.rs",
                true,
                "ChangeZoneAll: mass battlefield zone move (census); single ChangeZone path \
                 also present in-file",
            ),
            (
                "copy_token_blocking.rs",
                true,
                "CopyTokenBlockingAttacker: UNCONDITIONAL zone_object_ids(Battlefield) scan, \
                 one token copy per matching attacker, grows the board (CR 707.2)",
            ),
            (
                "counters.rs",
                true,
                "PutCounterAll (resolve_add_all) + MultiplyCounter (resolve_defined_or_\
                 targets, targets-empty) mass battlefield counter scans",
            ),
            (
                "goad.rs",
                true,
                "GoadAll: battlefield_phased_in_ids mass goad; single Goad path also present",
            ),
            (
                "mod.rs",
                true,
                "shared-helper HOME: defines resolved_battlefield_object_ids (prefer \
                 explicit chosen targets, else battlefield mass scan); consumers \
                 turn_face_up/down census",
            ),
            (
                "phase_out.rs",
                true,
                "PhaseOut/PhaseIn: targets-empty -> battlefield_phased_in_ids / \
                 state.battlefield mass scan (CR 702.26)",
            ),
            (
                "turn_face_up.rs",
                true,
                "TurnFaceUp: delegates to resolved_battlefield_object_ids (CR 708.2)",
            ),
            (
                "turn_face_down.rs",
                true,
                "TurnFaceDown: delegates to resolved_battlefield_object_ids (CR 708.2a)",
            ),
            // ---- RELAX (documented; NOT a scaling mass battlefield read) ----
            (
                "choose_damage_source.rs",
                false,
                "bounded selection: enumerates damage-source candidates \
                 (battlefield/stack/command, CR 609.7a) for a SINGLE chosen source",
            ),
            (
                "choose_from_zone.rs",
                false,
                "zone-disjoint / bounded selection from a named zone pool \
                 (library/graveyard/exile)",
            ),
            (
                "mana.rs",
                false,
                "bounded aggregate: distinct_colors_among_permanents returns <=5 colors, \
                 does not scale with board growth",
            ),
            (
                "perpetual.rs",
                false,
                "zone-disjoint: mass ApplyPerpetual path only over non-battlefield/hand \
                 zones (CR 601.2f); battlefield path is source/ParentTarget-bounded",
            ),
            (
                "search_outside_game.rs",
                false,
                "zone-disjoint: outside-the-game pool, not the battlefield growth class",
            ),
            (
                "token_copy.rs",
                false,
                "CopyTokenOf source_filter scan is scan_effect-CONSERVATIVE-vetoed (safe via \
                 the whole-effect conservative arm, not the census tag)",
            ),
        ];

        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/game/effects/");
        let mut matched: BTreeSet<String> = BTreeSet::new();
        for entry in std::fs::read_dir(dir).expect("read game/effects dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read effect source");
            if matches_idiom(&src) {
                matched.insert(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }

        let curated: BTreeSet<String> = CLASSIFIED.iter().map(|(f, _, _)| f.to_string()).collect();
        assert_eq!(
            matched, curated,
            "mass-battlefield-scan resolver set drifted from the curated classification. A \
             new/removed file matching the broadened idiom must be added to / removed from \
             CLASSIFIED with a Census|Relax verdict + reason (classify the flood, do NOT \
             re-narrow with an allowlist). This is the durable forward guard against the F1 \
             silent-miss class."
        );

        // Tie every Census file to the ORACLE: its representative mass Effect variant MUST
        // be a census member in `effect_census_role` (source-scanned, mirroring
        // census_partition_agrees). Reverting that variant's census-tag drops it from the
        // set -> this fails (non-vacuity ii). mod.rs is census-by-delegation; its consumers
        // turn_face_up/down are tied below.
        let src = include_str!("ability_scan.rs");
        let ecr_start = src.find("fn effect_census_role(").expect("ecr");
        let ecr = &src[ecr_start..ecr_start + src[ecr_start..].find("\n}\n").expect("ecr end")];
        let census_block = &ecr[..ecr
            .find("=> CensusRole::Census,")
            .expect("census terminator")];
        let census_names: BTreeSet<&str> = census_block
            .match_indices("Effect::")
            .map(|(i, _)| {
                let s = &census_block[i + "Effect::".len()..];
                let e = s
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(s.len());
                &s[..e]
            })
            .collect();
        let census_reps: &[(&str, &str)] = &[
            ("change_zone.rs", "ChangeZoneAll"),
            ("copy_token_blocking.rs", "CopyTokenBlockingAttacker"),
            ("counters.rs", "PutCounterAll"),
            ("goad.rs", "GoadAll"),
            ("phase_out.rs", "PhaseOut"),
            ("turn_face_up.rs", "TurnFaceUp"),
            ("turn_face_down.rs", "TurnFaceDown"),
        ];
        for (file, variant) in census_reps {
            assert!(
                CLASSIFIED.iter().any(|(f, census, _)| f == file && *census),
                "{file} must be curated as Census"
            );
            assert!(
                census_names.contains(variant),
                "census-classified {file}: its representative variant {variant} must be a \
                 census member in effect_census_role (reverting its tag breaks this tie)"
            );
        }
    }

    /// A7 byte-identity: the self-asserting `QuantityRef` board-census arms yield
    /// `sibling:true` in BOTH modes (their census read is mode-invariant), so the
    /// LoopFirewall `Typed` relaxation never touches them and CR 603.3b Conservative
    /// is unchanged. Includes the bug-(a) non-`Typed` case (census base covers it).
    #[test]
    fn aggregate_arms_byte_identical_in_conservative() {
        use crate::types::ability::QuantityRef;
        use ScanMode::{Conservative, LoopFirewall};
        let oc = QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter::creature()),
        };
        assert!(scan_quantity_ref(&oc, Conservative).sibling);
        assert!(scan_quantity_ref(&oc, LoopFirewall).sibling);
        let oc2 = QuantityRef::ObjectCount {
            filter: TargetFilter::Controller,
        };
        assert!(scan_quantity_ref(&oc2, Conservative).sibling);
        assert!(scan_quantity_ref(&oc2, LoopFirewall).sibling);
    }

    // ---- A2 determinism gate: the randomness classifier (CR 732.2a) ----
    #[test]
    fn randomness_classifier_discriminates() {
        use crate::types::ability::{
            AbilityKind, CardSelectionMode, ChoiceType, TargetSelectionMode,
        };

        // Effect-variant randomness (CR 705.1 / CR 706.1a) → true.
        assert!(effect_is_randomness_bearing(&Effect::FlipCoin {
            win_effect: None,
            lose_effect: None,
            flipper: TargetFilter::Controller,
        }));
        assert!(effect_is_randomness_bearing(&Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 6,
            results: Vec::new(),
            modifier: None,
        }));
        assert!(effect_is_randomness_bearing(&Effect::FlipCoinUntilLose {
            win_effect: Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        }));
        // Unit dice variants (planar / attraction / contraption) → true.
        assert!(effect_is_randomness_bearing(&Effect::ChaosEnsues));
        assert!(effect_is_randomness_bearing(
            &Effect::RollToVisitAttractions
        ));
        assert!(effect_is_randomness_bearing(
            &Effect::AssembleContraptionsFromRollDifference
        ));

        // Field-level Random selection (CR 701.9a) → true; Chosen → false. This
        // exercises the `.is_random()` wiring on the shared `CardSelectionMode` arm.
        let discard = |sel| Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            selection: sel,
            unless_filter: None,
            filter: None,
        };
        assert!(effect_is_randomness_bearing(&discard(
            CardSelectionMode::Random
        )));
        assert!(!effect_is_randomness_bearing(&discard(
            CardSelectionMode::Chosen
        )));
        // Momir (CreateTokenCopyFromPool) — same `CardSelectionMode` arm as Discard,
        // via a distinct card class.
        assert!(effect_is_randomness_bearing(
            &Effect::CreateTokenCopyFromPool {
                owner: TargetFilter::Controller,
                type_filter: TargetFilter::Any,
                mv: Comparator::EQ,
                mv_bound: QuantityExpr::Fixed { value: 0 },
                selection: CardSelectionMode::Random,
                count: QuantityExpr::Fixed { value: 1 },
                tapped: false,
                enters_attacking: false,
            }
        ));
        // Choose is the distinct `TargetSelectionMode`-carrier arm.
        assert!(effect_is_randomness_bearing(&Effect::Choose {
            choice_type: ChoiceType::OddOrEven,
            persist: false,
            selection: TargetSelectionMode::Random,
        }));
        assert!(!effect_is_randomness_bearing(&Effect::Choose {
            choice_type: ChoiceType::OddOrEven,
            persist: false,
            selection: TargetSelectionMode::Chosen,
        }));

        // Non-randomness effects → false. `Effect::Token` (the 51st's body) is
        // additionally proven not-over-rejected end-to-end by the paired-positive
        // integration test `object_growth_51st_sprout_swarm_covers_and_offers`.
        assert!(!effect_is_randomness_bearing(&Effect::NoOp));
        assert!(!effect_is_randomness_bearing(&Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        }));

        // CR 701.30a/d: a clash reveals the top card of a shuffled library and decides the winner
        // by comparing revealed mana values — unpredictable at pin time (CR 732.2a) ⇒ true.
        // Revert-probe: moving `Effect::Clash` back to the non-randomness arm flips this to false.
        assert!(effect_is_randomness_bearing(&Effect::Clash));
    }

    #[test]
    fn spell_ability_randomness_ability_level_and_tree() {
        use crate::types::ability::{AbilityKind, TargetSelectionMode};

        // Ability-level announce-time Random selection (CR 700.2b) on an otherwise
        // randomness-free body ⇒ true (proves the `target_selection_mode` axis is wired
        // independently of the effect-tree walk).
        let mut announce_random = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        announce_random.target_selection_mode = TargetSelectionMode::Random;
        assert!(spell_ability_bears_randomness(&announce_random));

        // Randomness reached only through the effect tree (via `collect_effects`) ⇒ true.
        let coin_body = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        );
        assert!(spell_ability_bears_randomness(&coin_body));

        // Deterministic body (Chosen announce mode, no random effect) ⇒ false.
        let plain = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        assert!(!spell_ability_bears_randomness(&plain));
    }

    // ---- Axis 3: projected-resource readers (must classify TRUE) ----
    #[test]
    fn projected_readers_classify_as_reading() {
        // Life axis (CR 119).
        assert!(ability_reads_projected_resource(&ability_with_amount(
            QuantityRef::LifeTotal {
                player: PlayerScope::Controller
            }
        )));
        // Player-counter axis (CR 122.1) — N1(n) walker pairing; experience has NO
        // winner-predicate firewall, so this classification is the only rejection.
        assert!(ability_reads_projected_resource(&ability_with_amount(
            QuantityRef::PlayerCounter {
                kind: PlayerCounterKind::Experience,
                scope: CountScope::Controller
            }
        )));
        // Per-turn life-gained journal.
        assert!(ability_reads_projected_resource(&ability_with_amount(
            QuantityRef::LifeGainedThisTurn {
                player: PlayerScope::Controller
            }
        )));
        // Cast journal (spells cast this turn, cleared by project_out_resources).
        assert!(ability_reads_projected_resource(&ability_with_amount(
            QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: None
            }
        )));
        // Damage journal (damage dealt this turn).
        assert!(ability_reads_projected_resource(&ability_with_amount(
            QuantityRef::DamageDealtThisTurn {
                source: Box::new(TargetFilter::Any),
                target: Box::new(TargetFilter::Any),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: crate::types::ability::DamageKindFilter::Any,
                channel: crate::types::ability::DamageChannel::Total,
            }
        )));
        // Trigger fire-time intervening-if readers.
        assert!(trigger_condition_reads_projected_resource(
            &TriggerCondition::GainedLife { minimum: 30 }
        ));
        assert!(trigger_condition_reads_projected_resource(
            &TriggerCondition::LifeTotalGE { minimum: 6 }
        ));
        // Ability-condition branch selector reading the per-ability resolution count.
        assert!(ability_condition_reads_projected_resource(
            &AbilityCondition::NthResolutionThisTurn { n: 10 }
        ));
        // Static-condition dormant reader (poison).
        assert!(static_condition_reads_projected_resource(
            &StaticCondition::OpponentPoisonAtLeast { count: 1 }
        ));
        // Replacement-condition dormant reader (life).
        assert!(replacement_condition_reads_projected_resource(
            &ReplacementCondition::UnlessPlayerLifeAtMost { amount: 5 }
        ));
        // Transient ForAsLongAs duration wrapping a life-reading static condition.
        assert!(duration_reads_projected_resource(&Duration::ForAsLongAs {
            condition: StaticCondition::OpponentPoisonAtLeast { count: 1 }
        }));
    }

    // ---- Axis 3: object/board readers are NON-reading (R5-B1 negative) ----
    #[test]
    fn object_and_board_readers_are_not_projected() {
        // Object counter / P/T reads are strict-compared by gate (1), not projected.
        for qty in [
            QuantityRef::Power {
                scope: ObjectScope::Source,
            },
            QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: None,
            },
            QuantityRef::ObjectCount {
                filter: TargetFilter::Any,
            },
        ] {
            assert!(!ability_reads_projected_resource(&ability_with_amount(qty)));
        }
        // Structural conditions do not read a projected axis.
        assert!(!trigger_condition_reads_projected_resource(
            &TriggerCondition::SourceIsTapped
        ));
        assert!(!static_condition_reads_projected_resource(
            &StaticCondition::SourceIsTapped
        ));
        assert!(!ability_condition_reads_projected_resource(
            &AbilityCondition::IsYourTurn
        ));
        assert!(!replacement_condition_reads_projected_resource(
            &ReplacementCondition::CastFromZone {
                zone: crate::types::zones::Zone::Graveyard
            }
        ));
        assert!(!duration_reads_projected_resource(
            &Duration::UntilEndOfTurn
        ));
        // The plain fixed drain reads nothing on any axis.
        assert!(!ability_reads_projected_resource(&fixed_drain()));
    }

    // ---- Axis 1: event-context ----
    #[test]
    fn event_context_axis_discriminates() {
        // "gain THAT MUCH life" reads the triggering event amount.
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::EventContextAmount
        )));
        // Fixed drain does not.
        assert!(!ability_uses_event_context(&fixed_drain()));

        // Each of the 5 event-context escapees, reached through a carrier the walk
        // actually traverses, must classify event == true.
        // (1) ObjectScope::EventSource via QuantityRef::Power.
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::Power {
                scope: ObjectScope::EventSource,
            }
        )));
        // (2) TargetFilter::TriggeringSourceController via QuantityRef::ObjectCount filter.
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::ObjectCount {
                filter: TargetFilter::TriggeringSourceController,
            }
        )));
        // (3) TargetFilter::ParentTargetSlot via QuantityRef::ObjectCount filter.
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::ObjectCount {
                filter: TargetFilter::ParentTargetSlot { index: 0 },
            }
        )));
        // (4) QuantityRef::TimesCostPaidThisResolution directly.
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::TimesCostPaidThisResolution
        )));
        // (5) CastManaObjectScope::TriggeringSpell via QuantityRef::ManaSpentToCast,
        //     whose whole arm is Axes::CONSERVATIVE (fail-closed ⇒ event == true).
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::TriggeringSpell,
                metric: CastManaSpentMetric::Total,
            }
        )));

        // Cross-axis negative: a purely projected-resource reader (life, CR 119)
        // does NOT read event context — the axes are independent.
        assert!(!ability_uses_event_context(&ability_with_amount(
            QuantityRef::LifeTotal {
                player: PlayerScope::Controller,
            }
        )));
    }

    // ---- BLOCKER 1 regression: multi_target bounds are traversed ----
    #[test]
    fn multi_target_bound_event_read_classifies() {
        // Base effect reads nothing; the ONLY event read is the multi_target min.
        // Revert-fail: drop the `multi_target` traversal ⇒ this flips to inert.
        let mut a = fixed_drain();
        a.multi_target = Some(MultiTargetSpec {
            min: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            max: None,
        });
        assert!(ability_uses_event_context(&a));
        // Sanity: without the multi_target it is inert (isolates the min bound).
        assert!(!ability_uses_event_context(&fixed_drain()));
    }

    // ---- BLOCKER 2 regression: target_constraints are traversed ----
    #[test]
    fn target_constraint_event_read_classifies() {
        // The ONLY read is the TotalManaValue where-X bound (EventContextAmount).
        // Revert-fail: drop the `target_constraints` traversal ⇒ this flips to inert.
        let mut a = fixed_drain();
        a.target_constraints = vec![TargetSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        }];
        assert!(ability_uses_event_context(&a));
        // Sanity: the Different* constraints carry no read.
        let mut b = fixed_drain();
        b.target_constraints = vec![TargetSelectionConstraint::DifferentTargetPlayers];
        assert!(!ability_uses_event_context(&b));
    }

    // ---- BB-FU10 Step 0c: the CR 608.2i ledger read is an axis-2 board read ----

    /// Build an `AbilityDefinition` whose effect magnitude is `qty`.
    fn ability_def_with_amount(qty: QuantityRef) -> crate::types::ability::AbilityDefinition {
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Ref { qty },
                player: TargetFilter::Controller,
            },
        )
    }

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![],
        })
    }

    /// T15 (BB-FU10 Step 0c). `battlefield_entries_this_turn` is APPENDED to by
    /// every battlefield entry (`record_battlefield_entry`), so a read of it is a
    /// board-derived AGGREGATE and must self-assert `sibling: true` — CR 732.2a:
    /// the object-growth firewall may only ever OVER-veto, never certify a loop as
    /// bounded while a live observer reads the growing class.
    ///
    /// VACUITY TRAP, named explicitly: assertion (1) is `ScanMode::Conservative`,
    /// where the `TargetFilter::Typed` arm already forces `sibling: true`. It
    /// passes with AND without Step 0c and is therefore NOT the discriminator.
    /// Assertion (2) — `ScanMode::LoopFirewall`, the mode the two production
    /// callers in `analysis::resource::fire_time_conditions_read_growing_class`
    /// use — is the one Step 0c actually moves.
    ///
    /// REVERT-PROBE: set the arm's `sibling` back to `false` → (2) and (3) FAIL
    /// while (1) still passes.
    #[test]
    fn bbfu10_ledger_ref_is_sibling_mutable_in_both_scan_modes() {
        let ledger = ability_def_with_amount(QuantityRef::BattlefieldEntriesThisTurn {
            player: PlayerScope::Controller,
            filter: creature_filter(),
        });
        let live = ability_def_with_amount(QuantityRef::EnteredThisTurn {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![crate::types::ability::TypeFilter::Creature],
                controller: Some(crate::types::ability::ControllerRef::You),
                properties: vec![],
            }),
        });

        // (1) Conservative — vacuity trap, true either way.
        assert!(
            ability_definition_reads_sibling_mutable(&ledger),
            "(1) Conservative axis-2 (CR 603.3b consumer) — passes with or without Step 0c",
        );
        // (2) THE DISCRIMINATOR — LoopFirewall, the CR 732.2a firewall's mode.
        assert!(
            ability_definition_reads_sibling_mutable_for_loop(&ledger),
            "(2) CR 732.2a: the ledger read must stay axis-2 under LoopFirewall — \
             this is the exact predicate analysis/resource.rs calls at the two \
             `..._for_loop` scan sites",
        );
        // (3) parity guard — the look-back and live siblings must agree on both axes.
        assert_eq!(
            (
                ability_definition_reads_sibling_mutable(&ledger),
                ability_definition_reads_sibling_mutable_for_loop(&ledger),
            ),
            (
                ability_definition_reads_sibling_mutable(&live),
                ability_definition_reads_sibling_mutable_for_loop(&live),
            ),
            "(3) parity: `BattlefieldEntriesThisTurn` and `EnteredThisTurn` are the \
             same board-aggregate class on the sibling axis",
        );
        // (4) the projected axis is untouched by Step 0c.
        assert!(
            resolved_ability_axes(
                &ability_with_amount(QuantityRef::BattlefieldEntriesThisTurn {
                    player: PlayerScope::Controller,
                    filter: creature_filter(),
                }),
                ScanMode::LoopFirewall,
            )
            .projected,
            "(4) `projected` was already true and stays true",
        );
    }

    /// T17 (BB-FU10 Step 0c, T16's non-vacuity instrument). Proves the SHIPPED
    /// Park Heights Pegasus face is what
    /// `analysis::resource::fire_time_conditions_read_growing_class` block (1)
    /// visits, and that the flip lands on the trigger `execute` scan rather than
    /// on the Conservative trigger-`condition` path that already forces
    /// `sibling: true` (the Gargoyle Flock trap: `true → true`, non-discriminating).
    ///
    /// REVERT-PROBE: set the ledger arm's `sibling` back to `false` → (2) FAILS.
    /// Measured: PRE-0c `false`, POST-0c `true`, with `condition.is_none()` in both.
    #[test]
    fn bbfu10_shipped_ledger_observer_flips_for_loop_axis() {
        let db = crate::test_support::shared_card_db();
        let face = db
            .face_index
            .get("park heights pegasus")
            .expect("Park Heights Pegasus is in tests/fixtures/integration_cards.json");

        // (1) the flip CANNOT be landing on the Conservative `condition` path.
        assert_eq!(face.triggers.len(), 1, "(1) exactly one trigger definition");
        assert!(
            face.triggers[0].condition.is_none(),
            "(1) no intervening-if condition, so `trigger_condition_reads_sibling_mutable` \
             (Conservative, always true for a Typed filter) is NOT what fires here",
        );
        let execute = face.triggers[0]
            .execute
            .as_deref()
            .expect("(1) the trigger must carry an execute body");

        // (3) reach-guard — the body really contains the ledger read.
        let rendered = serde_json::to_string(execute).expect("AbilityDefinition serializes");
        assert!(
            rendered.contains("BattlefieldEntriesThisTurn"),
            "(3) reach-guard: the scanned body must carry the CR 608.2i ledger read, \
             not some other board aggregate",
        );

        // (2) THE DISCRIMINATOR — literally the callee at the block-(1) scan site.
        assert!(
            ability_definition_reads_sibling_mutable_for_loop(execute),
            "(2) CR 732.2a: a shipped ledger observer must veto an object-growth \
             certificate — reads `false` without Step 0c",
        );

        // (4) negative sibling — a plain draw trigger body does NOT veto.
        let plain = crate::parser::parse_oracle_text(
            "Whenever this creature deals combat damage to a player, draw a card.",
            "Bbfu10 Plain Draw Trigger",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let plain_execute = plain
            .triggers
            .first()
            .and_then(|t| t.execute.as_deref())
            .expect("(4) the plain trigger must parse an execute body");
        assert!(
            !ability_definition_reads_sibling_mutable_for_loop(plain_execute),
            "(4) negative sibling: a fixed draw reads no board aggregate",
        );
    }

    // ---- Axis 2: sibling-mutable board read (Rubblebelt / Orcish class) ----
    #[test]
    fn sibling_mutable_axis_discriminates() {
        // A board-count-scaled pump reads a mutable aggregate a sibling could change.
        assert!(ability_reads_sibling_mutable(&ability_with_amount(
            QuantityRef::ObjectCount {
                filter: TargetFilter::Any
            }
        )));
        // Source power (Orcish Siegemaster class) is a sibling-mutable read.
        assert!(ability_reads_sibling_mutable(&ability_with_amount(
            QuantityRef::Power {
                scope: ObjectScope::Source
            }
        )));
        // Fixed drain reads no sibling-mutable state — safe to auto-resolve.
        assert!(!ability_reads_sibling_mutable(&fixed_drain()));
    }

    // ---- Resolution-time choice classifier: pinned in BOTH directions ----
    /// Guard test (9092a8961 standard): pins `effect_resolution_choice_freedom`
    /// and the ability-level wrapper flips.
    ///
    /// The `FreeUnlessLifeReplacements` allow set is EXACTLY
    /// `{Effect::GainLife, Effect::LoseLife}` — asserted below and pinned by the
    /// allow-arm census (`rg -c 'ResolutionChoiceFreedom::FreeUnlessLifeReplacements'
    /// ability_scan.rs` == 2, both inside `effect_resolution_choice_freedom`). A
    /// future third allow arm must update this pin, the census, and add a
    /// resolver-trace grounding row.
    ///
    /// Compiler-exhaustiveness leg: `effect_resolution_choice_freedom`'s match has
    /// no wildcard catch-all, so a NEW `Effect` variant fails to compile until classified.
    /// Executed revert-fail (documented in the commit): classifying `Effect::Scry`
    /// ⇒ `FreeUnlessLifeReplacements` turns this test RED.
    #[test]
    fn resolution_choice_verdicts_are_exactly_pinned() {
        use crate::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, UnlessPayModifier,
        };
        use ResolutionChoiceFreedom::{FreeUnlessLifeReplacements, MayPrompt};

        // Allow-list (soundness claims) ⇒ FreeUnlessLifeReplacements.
        let gain = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        };
        let lose = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        };
        assert_eq!(
            effect_resolution_choice_freedom(&gain),
            FreeUnlessLifeReplacements
        );
        assert_eq!(
            effect_resolution_choice_freedom(&lose),
            FreeUnlessLifeReplacements
        );

        // Reject side: the finding's kinds + adjacent siblings ⇒ MayPrompt, each
        // with its resolver-prompt raise-site citation.
        let rejects = [
            Effect::Proliferate, // WaitingFor::ProliferateChoice — proliferate.rs:109
            Effect::Populate,    // WaitingFor::PopulateChoice — populate.rs:50
            Effect::Clash,       // WaitingFor::ClashChooseOpponent — clash.rs:47
            Effect::Behold {
                filter: TargetFilter::Any,
            }, // WaitingFor::BeholdChoice — behold.rs (2+ candidates)
            Effect::Explore,     // WaitingFor::ExploreChoice — explore.rs:191
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }, // Scry always prompts (bottom/top ordering)
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            }, // WaitingFor::EffectZoneChoice — sacrifice.rs:306
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            }, // discard selection prompt
        ];
        for e in &rejects {
            assert_eq!(
                effect_resolution_choice_freedom(e),
                MayPrompt,
                "{e:?} must be MayPrompt"
            );
        }

        // Explicit allow-set pin: exactly {GainLife, LoseLife}. Every other kind
        // sampled above is on the reject side; the allow-arm census is the
        // structural guard against a silent third allow arm.
        assert!(
            rejects
                .iter()
                .all(|e| effect_resolution_choice_freedom(e) == MayPrompt),
            "the FreeUnlessLifeReplacements set is exactly {{Effect::GainLife, Effect::LoseLife}}"
        );

        // Ability-level wrapper flips: base ⇒ Free (paired positive reach-guard),
        // each single-field mutation ⇒ MayPrompt (proves the FLIP, not something
        // upstream, causes the rejection).
        let base = ResolvedAbility::new(gain.clone(), Vec::new(), ObjectId(1), PlayerId(0));
        assert_eq!(
            ability_resolution_choice_freedom(&base),
            FreeUnlessLifeReplacements
        );

        let mut a = base.clone();
        a.optional = true;
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.optional_targeting = true;
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::Tap,
            payer: TargetFilter::Controller,
        });
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.target_chooser = Some(TargetFilter::Controller);
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.target_choice_timing = TargetChoiceTiming::Resolution;
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.mode_abilities = vec![AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)];
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.repeat_until = Some(RepeatContinuation::ControllerChoice);
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);

        let mut a = base.clone();
        a.modal = Some(ModalChoice::default());
        assert_eq!(ability_resolution_choice_freedom(&a), MayPrompt);
    }
}

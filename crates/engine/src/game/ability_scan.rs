//! CR 603.3b + CR 603.4 + CR 106.1 / CR 119 / CR 122.1: the fail-closed AST
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
//!    `loop_states_cover_modulo_growth`, so an object-counter reader
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
//! # Traversal closure
//!
//! The compiler-exhaustiveness floor holds only for TRAVERSED subtrees: an
//! untraversed payload is silently skipped with no compile error, so the traversal
//! set is part of the trusted base. It is closed under payload reachability across
//! `Effect`, `QuantityRef`, `QuantityExpr`, `AbilityCondition`, `TargetFilter`,
//! `ObjectScope`, `TriggerCondition`, `TriggerDefinition`, `DelayedTriggerCondition`,
//! `Duration` (its `ForAsLongAs` `StaticCondition`), `StaticCondition`, `PlayerFilter`,
//! `ReplacementCondition`, the target-count and target-set specs (`MultiTargetSpec`,
//! `TargetSelectionConstraint`), the loop and modal headers (`RepeatContinuation`,
//! `ModalChoice`), and the player scope selectors (`PlayerScope`, `ControllerRef`,
//! `CountScope`). The `ResolvedAbility` and `ModalChoice` fields are destructured
//! without `..`, so a new field must be classified before it compiles. Any type outside
//! this set that can reach a read is in the conservative set above.
//!
//! # Resolution-time choice classifier — LIVES IN `game::resolution_prompt`
//!
//! An independent classifier answering a FOURTH, orthogonal question
//! (CR 608.2d) — can resolving this ability enter a resolution-time player
//! choice (a non-priority `WaitingFor`)? — used to live here. It now lives in
//! `crate::game::resolution_prompt`, because answering it requires PROBING a
//! resolution and therefore requires a live board, which this module
//! deliberately never holds (pinned by
//! `resolution_prompt::tests::ability_scan_holds_no_game_state`, which asserts
//! this file carries no word-bounded board-type token at all — including in
//! this very sentence, which is why it is worded around the name).
//!
//! It is deliberately NOT a fourth `Axes` axis — `Axes::NONE` means "no reads",
//! which is orthogonal to "never prompts" (`Effect::Scry` reads nothing yet
//! always prompts), so folding a choice bit into `Axes` would make every
//! existing `NONE` arm silently claim choice-freeness.
//!
//! # Consumers of the read-axis classifiers
//!
//! CR 603.3b: the legacy UNGATED trigger-ordering paths (same firing event, and
//! the explicitly-simultaneous ZoneChanged departure batch) no longer consume the
//! event-context / sibling-mutable read classifiers of this scanner. They consume
//! the richer kind/scope read/write conflict profile in the sibling module
//! `ability_rw.rs` (`ability_rw_profile` / `trigger_condition_rw_profile` /
//! `profiles_conflict`), which answers "which kinds of state does the ability READ
//! and WRITE, at what scope" — the precise read/write predicate those paths
//! require. The event-context and sibling-mutable read classifiers here are now
//! consumed ONLY by the distinct-event term (`group_is_order_independent` /
//! `trigger_events_match_for_ordering`), ungated from loop detection and conjoined
//! with `!batch_conflict` — so a coarse distinct-event verdict may
//! auto-order a distinct-event group only when the precise `ability_rw` profiler also
//! agrees it is conflict-clean; a conservative verdict here means a prompt (safe
//! over-reject). The projected-resource classifier (question 3) and the
//! resolution-time choice classifier (question 4) are unchanged. See `ability_rw.rs`
//! for the conflict model and its CR 603.3b commutation argument.

use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, CardTypeSetSource, ContinuousModification,
    ControllerRef, CountScope, DelayedTriggerCondition, Duration, EachDamageRecipient, Effect,
    EffectScope, FilterProp, ForEachCategoryAction, GuessSubject, KeeperConstraint, ManaProduction,
    ModalChoice, MultiTargetSpec, ObjectScope, PlayerFilter, PlayerScope, PtValue, QuantityExpr,
    QuantityRef, ReciprocalZoneChoiceRole, RepeatContinuation, ReplacementCondition,
    ResolvedAbility, StaticCondition, TargetFilter, TrackedAnaphorSource, TriggerCondition,
    TriggerConstraint, TriggerDefinition, TypedFilter, UnlessPayModifier, ZoneChangeClause,
    ZoneChoiceCandidateSource,
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

    /// CR 732.2a: a shortcut proposal is legal only on outcomes the loop's own progress cannot
    /// move.
    /// `sibling` is the board half — a growing class moves a board aggregate.
    /// CR 106.1 / CR 119 / CR 122.1: `projected` is the player half — the monotone resources and
    /// per-turn journals `analysis::resource::project_out_resources` neutralizes, so the loop cover
    /// cannot see a read of one.
    /// A `LoopFirewall` consult must ask both. The CR 603.3b trigger-ordering gate
    /// (`game::triggers`) asks a different question and consults `.event` and `.sibling`
    /// as separate single-axis projections, never this disjunction.
    fn reads_growing_class(self) -> bool {
        self.sibling || self.projected
    }

    fn or(self, other: Axes) -> Axes {
        Axes {
            event: self.event || other.event,
            sibling: self.sibling || other.sibling,
            projected: self.projected || other.projected,
        }
    }
}

/// Which consumer is asking, and thus how a MODE-DIVERGENT arm classifies.
/// An arm is mode-divergent exactly when its body branches on this enum;
/// that set grows, so it is not enumerated here — `match mode` is its own
/// index.
///
/// `Conservative` is the pre-existing shared answer that the CR 603.3b
/// trigger-ordering gate (`game::triggers`) and every non-firewall caller
/// require, and it keeps the `LoopDetectionMode::Off` game byte-identical
///. `LoopFirewall` is used ONLY by the CR 732.2a object-growth
/// firewall (`analysis::resource`), which needs a mode-divergent arm to
/// DESCEND rather than fail closed. `Off` cannot observe `LoopFirewall`:
/// this value is constructed nowhere outside this module, inside the
/// `pub(crate)` entry points whose only production consumer is that
/// firewall, itself reachable only under `loop_detection.samples()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanMode {
    /// Fail-closed on every mode-divergent arm (the shared CR 603.3b + default answer).
    Conservative,
    /// Descend a mode-divergent arm's payload (CR 732.2a firewall only).
    LoopFirewall,
}

/// How a given `scan_target_filter` CALL SITE reads its filter — the census
/// discipline the CR 732.2a object-growth firewall's `Typed` relaxation depends on.
/// Analysis plumbing (a sibling of [`ScanMode`]), NOT a game-semantic variant: it
/// records whether the caller counts/tests LIVE battlefield membership (a board
/// census whose `sibling` read is its OWN — never inherited from the filter, never
/// relaxed) or names a snapshot / triggering event / single-object target (where
/// `sibling` may only come from a genuine board-reading component of the filter). A
/// REQUIRED parameter with NO `Default` impl, so no caller can obtain a filter's
/// axes without stating its census intent.
///
/// **Default to census:** an AMBIGUOUS or newly-added call site is
/// [`FilterReadContext::LiveBoardCensus`]. `LiveBoardCensus` ⇒ `sibling:true` ⇒ VETO
/// and `SnapshotOrEvent` ⇒ relaxed ⇒ may OFFER, so a misjudgment toward census can
/// only OVER-veto (CR 732.2a: a coarse relation may reject, never accept).
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
        cast_occurrence: _,        // finalized-cast provenance, no dynamic read
        source_incarnation: _,     // self-transform epoch latch, no dynamic read
        noted_mana_payment: _,     // concrete activation-payment snapshot, no dynamic read
        trigger_source: _,         // exact triggered-source authority, no dynamic read
        trigger_definition_ref: _, // exact trigger occurrence, no dynamic read
        force_block_attacker: _,   // exact force-block referent, no dynamic read
        target_incarnations: _,    // CR 400.7 referent pins, no dynamic read
        selected_target_incarnations: _, // CR 400.7 selected-target pins, no dynamic read
        controller: _,             // player id
        original_controller: _,    // player id
        scoped_player: _,          // player id (iteration binding)
        kind: _,                   // AbilityKind tag (no payload)
        context: _,                // SpellContext: cast-time fact snapshot, not a live read
        optional_targeting: _,     // bool
        optional: _,               // bool
        optional_player,
        optional_for: _,         // OpponentMayScope: AnyOpponent/AnyPlayer, no read
        target_choice_timing: _, // Stack/Resolution tag
        description: _,          // display string
        selected_mode_labels: _, // display strings, no dynamic read
        // CR 700.2: mode-root position marker. Read-FREE on every scan axis: it
        // selects nothing from game state, it only says "a new instruction starts
        // here". The instructions themselves are `effect`/`sub_ability`, already
        // scanned above, so the axes of a chain are identical with or without it.
        modal_instruction_ordinal: _,
        // CR 608.2c: structural record of what a chain SPLIT detached. Read-FREE:
        // it selects nothing from game state and gates only whether a producer
        // may publish its population, which can narrow but never widen.
        detached_remainder: _,
        min_x_value: _,                  // u32
        cant_be_copied: _,               // bool
        copy_count_status: _,            // status tag
        forward_result: _,               // bool
        distribution: _,                 // concrete pre-assigned (TargetRef, u32) portions
        chosen_x: _,                     // concrete cast-time X
        cost_paid_object: _,             // concrete captured-object snapshot
        cost_paid_object_ids: _,         // concrete captured-object ids
        effect_context_object: _,        // concrete captured-object snapshot
        amassed_army_object: _,          // concrete captured-object snapshot
        ability_index: _,                // usize provenance
        may_trigger_origin: _,           // provenance tag
        target_selection_mode: _,        // Chosen/Random tag
        chosen_players: _,               // concrete chosen player ids
        replacement_applied: _,          // replacement provenance set, no dynamic read
        sub_link: _,                     // SubAbilityLink kind tag
        sibling_condition: _,            // SiblingCondition replication marker, no dynamic read
        distribute: _, // announcement unit tag/string, no resolution-time dynamic read
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
    if let Some(player) = optional_player {
        acc = acc.or(scan_target_filter(
            player,
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

/// Classify the candidate provenance of a zone choice on the read axes.
///
/// `Direct` reads only the named zone, so only a battlefield population can
/// grow with the loop class. `Tracked` and `Legacy` can instead consume the
/// current resolution-chain set, whose producer is not visible at this local
/// effect node; classify both fail-closed. This is intentionally separate from
/// the choice filter, which is scanned by the caller.
fn scan_zone_choice_candidate_source(
    candidate_source: ZoneChoiceCandidateSource,
    zone: crate::types::zones::Zone,
) -> Axes {
    match candidate_source {
        ZoneChoiceCandidateSource::Direct => {
            if zone == crate::types::zones::Zone::Battlefield {
                Axes {
                    event: true,
                    sibling: true,
                    projected: false,
                }
            } else {
                Axes::NONE
            }
        }
        ZoneChoiceCandidateSource::Tracked | ZoneChoiceCandidateSource::Legacy => {
            Axes::CONSERVATIVE
        }
    }
}

/// A reciprocal consumer reads the fresh set published by its producer. The
/// producer writes that continuation-local set but performs no corresponding
/// read, so it is read-free on this walk's three axes.
fn scan_reciprocal_zone_choice_role(role: Option<ReciprocalZoneChoiceRole>) -> Axes {
    match role {
        None | Some(ReciprocalZoneChoiceRole::Produce) => Axes::NONE,
        Some(ReciprocalZoneChoiceRole::Consume) => Axes::CONSERVATIVE,
    }
}

fn scan_effect(x: &Effect, mode: ScanMode) -> Axes {
    // The census discipline for THIS effect's target reads, derived ONCE
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
        // CR 101.4: publishes an already-committed per-player number. Writes only
        // the visibility half of the chosen-number ledger (`Number` ->
        // `RevealedNumber`), never a value, so it perturbs no scanned axis; the
        // player set it names is the only thing to descend into.
        Effect::RevealChosenNumbers { players } => scan_player_filter(players, mode),
        Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(sources, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(amount, mode));
            match recipient {
                EachDamageRecipient::Shared(filter) => {
                    acc = acc.or(scan_target_filter(filter, target_ctx, mode));
                }
                EachDamageRecipient::OtherBatchSource { source_filters } => {
                    for filter in source_filters {
                        acc = acc.or(scan_target_filter(filter, target_ctx, mode));
                    }
                }
                EachDamageRecipient::EachController => {}
            }
            acc
        }
        Effect::Draw { count, target } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        // CR 732.2a is the OPERATIVE rule for this arm, and the mode split is the one
        // `Effect::Token` and `Effect::Mana` already ship: under `Conservative` this stays
        // the byte-identical blanket every non-firewall consumer already sees; under
        // `LoopFirewall` it DESCENDS, because CR 732.2a admits a proposal only on "the
        // predictable results of the sequence of choices" and bars "conditional actions,
        // where the outcome of a game event determines the next action a player takes".
        //
        // RELIEF is an AST property, not a rules one: two literal `PtValue`s and a
        // read-free target mean nothing later becomes conditional on how far the loop ran.
        // VETO is where CR 608.2h bites — an effect requiring "information from the game
        // (such as the number of creatures on the battlefield)" has its answer "determined
        // only once, when the effect is applied", so each iteration re-determines it
        // against a LARGER board or a DIFFERENT life total (CR 208.1: power and toughness
        // carry the read independently). Exhaustive 3-field destructure, NO `..`.
        Effect::Pump {
            power,
            toughness,
            target,
        } => match mode {
            ScanMode::Conservative => Axes::CONSERVATIVE,
            ScanMode::LoopFirewall => {
                let mut acc = Axes::NONE;
                acc = acc.or(scan_pt_value(power, mode));
                acc = acc.or(scan_pt_value(toughness, mode));
                // `target_ctx` is `effect_target_ctx(x, mode)`, computed once at the head
                // of this function; it classifies `Effect::Pump` into the bounded
                // `SnapshotOrEvent` group. The same classification is what
                // `effect_target_reads_growing_class_for_loop` derives for
                // `analysis::resource`'s `pump_aggregate_provably_excludes_class` relief.
                acc = acc.or(scan_target_filter(target, target_ctx, mode));
                // The `projected` axis is not re-raised here and the verdict stays precise:
                // the def-level and effect-target entry points both ask
                // `Axes::reads_growing_class` (the disjunction), and the two
                // `continuous_modification_reads_*` entry points project one axis each with
                // their caller disjoining them — so `{sibling: false, projected: true}`
                // vetoes at every consumer, while an `if acc.projected` escalation would
                // make this arm report a sibling read the def does not have.
                //
                // The walk carries that axis because nothing downstream re-checks it here:
                // `fire_time_conditions_read_projected_resource_scoped` scans trigger
                // CONDITIONS, replacement conditions and bodies, static conditions and
                // transient effects — never `obj.abilities`, never a trigger `execute`
                // body; its `AbilityDefinition` walker runs only on a replacement's
                // `runtime_execute` and fails closed on `def.execute.is_some()`.
                acc
            }
        },
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
                // A granted static's condition + its layered modifications.
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
        Effect::ChooseCounterKind { target, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        Effect::PutChosenCounter {
            target,
            count,
            target_condition,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            if let Some(condition) = target_condition {
                acc = acc.or(scan_quantity_expr(&condition.rhs, mode));
            }
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
            rest_order: _,
            reveal: _,
            enter_tapped: _,
            enters_attacking: _,
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
        // CR 725.1 + CR 115.1: the designation subject is a target filter,
        // walked through the same single authority every other targeted effect
        // uses.
        Effect::BecomeMonarch { target } => scan_target_filter(target, target_ctx, mode),
        Effect::NoOp => Axes::NONE,
        // Captured at activation time; no resolution-time dynamic read.
        Effect::NoteManaSpent => Axes::NONE,
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
        // CR 707.2c: the chosen creature's copiable values are latched onto the
        // Aura's host at the answer — a copy-family continuous effect, same
        // conservative classification as `BecomeCopy`. `filter` scans no
        // per-source projected resource (it just bounds the choice pool).
        Effect::ChoosePermanent { filter } => {
            scan_target_filter(filter, target_ctx, mode).or(Axes::CONSERVATIVE)
        }
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
        // CR 122.1 + CR 603.2c: the per-kind magnitude is event-derived (not a
        // `QuantityExpr`), so only the reproduction target is scanned; mirrors
        // `MultiplyCounter`.
        Effect::ReproduceEventCounters {
            target,
            per_kind_count: _,
        } => {
            let mut acc = Axes::NONE;
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
                // CR 601.2c: a mana target is role-tagged (recipient / count
                // source). Scan EVERY declared role filter, mirroring the
                // legacy scan (`ability_rw`) and the AI POISON scan
                // (`ai_support::filter`) — a partial view here would let the
                // loop firewall miss a target-derived axis.
                if let Some(role) = target {
                    for (_, filter) in role.declared_filters() {
                        acc = acc.or(scan_target_filter(
                            filter,
                            FilterReadContext::SnapshotOrEvent,
                            mode,
                        ));
                    }
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
        Effect::Transform { target, .. } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc
        }
        // CR 710.4: identical scan shape to `Transform` — the only read is the
        // effect's own target filter.
        Effect::FlipPermanent { target } => {
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
        // CR 400.11b: identical scan shape to `SearchOutsideGame` — the only
        // reads are the effect's own candidate filter and its take count. The
        // pack's contents come from the shelf, not from board state.
        Effect::OpenBoosterPack {
            filter,
            count,
            destination: _,
            reveal: _,
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
            position: _,
            face_down: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
            acc = acc.or(scan_quantity_expr(count, mode));
            acc
        }
        Effect::ExileFaceDownPile {
            object,
            player,
            count,
        } => scan_target_filter(object, target_ctx, mode)
            .or(scan_target_filter(player, target_ctx, mode))
            .or(scan_quantity_expr(count, mode)),
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
        Effect::ForceBlock {
            target,
            attacker: _,
            duration,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_duration(duration, mode));
            acc
        }
        Effect::ForceAttack {
            target,
            required_defender,
            duration,
            // A static single-vs-mass discriminant (CR 115.1) — no event, sibling,
            // or projected-resource axis; the filters it selects between are
            // classified below.
            scope: _,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(required_defender, target_ctx, mode));
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
        // CR 732.2a is the OPERATIVE rule for this arm, and the mode split is the same one
        // the sibling descending arms already ship: under `Conservative` this stays the
        // byte-identical blanket the CR 603.3b ordering gate and every non-firewall consumer
        // already see. The tracked-set guard fails CLOSED — `true` resolves the payload
        // against the PARENT ability's tracked object set, a referent this definition cannot
        // see. CR 608.2h is operative on the body leg: a delayed body scaled by a board
        // aggregate "requires information from the game", determined once per application, so
        // each loop iteration re-determines it against a larger board. That body's `cost` is
        // never PAID: a delayed triggered ability is CREATED by an effect (CR 603.7) and is
        // not activated (CR 603.2a), so the CR 602.1a activation cost it holds has no payer,
        // as for the granted carrier's `execute` body. `projected` is NOT
        // re-raised here — the def-level entry point asks `Axes::reads_growing_class` and the
        // single-axis modification entry points are disjoined by their caller — so a
        // `projected`-only verdict is precise here and a veto there.
        Effect::CreateDelayedTrigger {
            condition,
            effect,
            uses_tracked_set,
        } => match mode {
            ScanMode::Conservative => Axes::CONSERVATIVE,
            ScanMode::LoopFirewall => {
                if *uses_tracked_set {
                    Axes::CONSERVATIVE
                } else {
                    let mut acc = scan_delayed_trigger_condition(condition, mode);
                    acc = acc.or(ability_definition_axes(effect, mode));
                    acc
                }
            }
        },
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
            graveyard_replacement: _,
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
            zone,
            filter,
            candidate_source,
            reciprocal_role,
            ..
        } => {
            let mut acc = Axes::NONE;
            if let Some(x) = filter {
                acc = acc.or(scan_target_filter(x, target_ctx, mode));
            }
            acc = acc.or(scan_zone_choice_candidate_source(*candidate_source, *zone));
            acc = acc.or(scan_reciprocal_zone_choice_role(*reciprocal_role));
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
        // NOT the growing class. Effect-level fields destructured no-`..` (a new
        // census field forces re-audit here via E0027). The inner `action` is
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
            chooser, filter, ..
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
            kept_destination_if,
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
            // CR 608.2c: the per-hit conditional destination filter (Part in
            // Friendship's "if its mana value is <= the number of lands you
            // control") reads game state exactly like the primary `filter` —
            // scan it identically.
            if let Some((cond_filter, _zone)) = kept_destination_if {
                acc = acc.or(scan_target_filter(cond_filter, target_ctx, mode));
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
        Effect::PutOnTopOrBottom { target, chooser } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_target_filter(target, target_ctx, mode));
            acc = acc.or(scan_target_filter(chooser, target_ctx, mode));
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
            object_source,
            enters_under,
            profile: _,
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
        Effect::Amass {
            count,
            subtype: _,
            player,
        } => {
            let mut acc = Axes::NONE;
            acc = acc.or(scan_quantity_expr(count, mode));
            acc = acc.or(scan_target_filter(player, target_ctx, mode));
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
        Effect::CompletePlayerAction { .. } => Axes::NONE,
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

fn scan_property_aggregate_source(source: &CardTypeSetSource, mode: ScanMode) -> Axes {
    let mut axes = Axes::NONE;
    let complete =
        source.try_for_each_member(crate::types::ability::UNION_DEPTH_BUDGET, &mut |leaf| {
            axes = axes.or(match leaf {
                CardTypeSetSource::Objects { filter } => Axes {
                    event: false,
                    sibling: true,
                    projected: false,
                }
                .or(scan_target_filter(
                    filter,
                    FilterReadContext::LiveBoardCensus,
                    mode,
                )),
                CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::TriggeringBatch,
                    ..
                } => Axes {
                    event: true,
                    sibling: false,
                    projected: false,
                },
                CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::ChainSet,
                    ..
                } => Axes::NONE,
                // CR 603.3b: the journal population is projected turn state,
                // while an optional event-relative filter still reads the
                // triggering event and must participate in trigger ordering.
                CardTypeSetSource::TurnJournal { filter, .. } => Axes {
                    event: false,
                    sibling: false,
                    projected: true,
                }
                .or(filter.as_ref().map_or(Axes::NONE, |filter| {
                    scan_target_filter(filter, FilterReadContext::SnapshotOrEvent, mode)
                })),
                CardTypeSetSource::Zone { .. } | CardTypeSetSource::ExiledBySource => {
                    Axes::CONSERVATIVE
                }
                CardTypeSetSource::AnyOf { .. } => Axes::NONE,
            });
        });
    if complete {
        axes
    } else {
        Axes::CONSERVATIVE
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
        // CR 701.22a + CR 603.2c: reads the current trigger's preserved event
        // (`state.current_trigger_event` — the scry's own `PlayerPerformedAction`
        // carrying its effective look count) → event axis true, mirroring
        // `QuantityRef::EventContextAmount` below.
        QuantityRef::TriggeringScryLookCount | QuantityRef::TriggeringScryBottomCount => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
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
        QuantityRef::Power { scope, .. } | QuantityRef::BasePower { scope, .. } => {
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
        QuantityRef::PropertyAggregate(aggregate) => {
            scan_property_aggregate_source(aggregate.source(), mode)
        }
        QuantityRef::ControlledByEachPlayer {
            filter,
            aggregate: _,
            relation: _,
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
        // Deliberately coarse: `Axes::CONSERVATIVE` is FAIL-CLOSED, so a new
        // `CardTypeSetSource` variant reached through this compiler-blind `{ .. }`
        // pattern can only over-report, never under-report. The population axis is
        // not decomposed here because no caller needs a narrower answer.
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
        QuantityRef::ExiledFromHandThisResolution => Axes::NONE,
        // CR 608.2c + CR 608.2i: every channel and every aggregate reads
        // resolution-local state — `last_effect_amount` /
        // `last_effect_excess_amount` / `last_effect_counts_by_player` /
        // `clause_minimum_snapshot`, the last read FIRST (`game/quantity.rs`,
        // the `PreviousEffectAmount` arm) as the CR 608.2h frozen value. All are
        // cleared at depth-0 chain entry (`resolve_ability_chain`); `apply()`
        // additionally clears `last_effect_count` and the per-player table at
        // every player action. None is a triggering-event characteristic
        // (event), a board-scoped mutable aggregate a sibling copy could mutate
        // (sibling), or a player-level per-turn projected resource (projected).
        // Destructured without `..` so a future field forces re-classification.
        QuantityRef::PreviousEffectAmount {
            channel: _,
            aggregate: _,
        } => Axes::NONE,
        QuantityRef::PreviousEffectCount => Axes::NONE,
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
        QuantityRef::SpellsCastBeforeTriggeringSpell { scope, filter } => {
            let mut acc = Axes {
                event: true,
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
            // CR 732.2a: axis-2 self-assertion. `record_battlefield_entry`
            // (game/restrictions.rs) APPENDS to `battlefield_entries_this_turn` on every
            // battlefield entry, so this tally is a board-derived AGGREGATE that a
            // sibling resolution or a loop cycle changes. `project_out_resources`
            // already clears it as an append-only event journal a loop pumps (CR 400
            // zones / CR 603.6a ETB / CR 701.21 sacrifice / CR 111 tokens).
            //
            // Per the ⛔ INVARIANT on `scan_target_filter`'s `Typed` arm, this
            // board-AGGREGATE caller self-asserts `sibling: true` rather than
            // delegating: the `Typed` arm's `LoopFirewall` relaxation would otherwise
            // erase the signal at `fire_time_conditions_read_growing_class`, which has
            // no `projected` twin. The CR 603.3b ordering gate is unaffected (it scans
            // `Conservative`), and the `filter` census intent stays `SnapshotOrEvent`:
            // it is matched against a `BattlefieldEntryRecord`, never a live board.
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
        // CR 101.4 + CR 608.2d: the number a player chose this resolution. Like
        // its object-axis sibling `ChosenNumber` this is a bounded one-shot
        // answer, not an accumulating projected resource — a re-choose REPLACES
        // the stored value rather than adding to it (`bind_named_choice`), so it
        // cannot grow across loop iterations. The only axis it can contribute is
        // whatever its player scope carries.
        QuantityRef::PlayerChosenNumber { player } => scan_player_scope(player),
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
        QuantityRef::DistinctColorsAmong { source } => match source {
            // CR 105.1 + CR 109.2: unchanged classification for the live board
            // census — the only population this head could name before it was
            // parameterized onto the shared axis.
            crate::types::ability::CardTypeSetSource::Objects { filter } => {
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
            // Zone / linked-exile / tracked-set / turn-journal / union
            // populations are classified like their card-type and subtype peers
            // above: `Axes::CONSERVATIVE`, which is fail-closed.
            crate::types::ability::CardTypeSetSource::Zone { .. }
            | crate::types::ability::CardTypeSetSource::ExiledBySource
            | crate::types::ability::CardTypeSetSource::TrackedSet { .. }
            | crate::types::ability::CardTypeSetSource::TurnJournal { .. }
            | crate::types::ability::CardTypeSetSource::AnyOf { .. } => Axes::CONSERVATIVE,
        },
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
        AbilityCondition::TriggerEventTargetDamagedBySourceThisTurn => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
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
        // CR 903.3d: "controlling a commander" is a permanent ON THE BATTLEFIELD
        // that is a commander — a live board census (`game::commander` scans
        // `state.battlefield` for `is_commander && controller == you [&& owner ==
        // you] && is_phased_in`), so a sibling copy that moves, steals or phases a
        // commander can flip this gate (CR 603.3b ordering-relevance). Self-asserts
        // its own `sibling: true` literal, as the ⛔ INVARIANT on
        // `scan_target_filter`'s `Typed` arm requires of every board-aggregate
        // caller. `event` stays false: the census reads no triggering-event
        // characteristic. `ownership: _` is destructured explicitly (as the
        // `TriggerCondition` mirror does) so a future field forces a re-audit here.
        AbilityCondition::ControlsCommander { ownership: _ } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
        // CR 309.7: controller-state predicate — touches no scan axis.
        AbilityCondition::CompletedDungeon { .. } => Axes::NONE,
        AbilityCondition::IsInitiative => Axes::NONE,
        AbilityCondition::HasCityBlessing => Axes::NONE,
        AbilityCondition::HasEnduringStory => Axes::NONE,
        AbilityCondition::DiscardedCardMatchesFilter { filter } => {
            scan_target_filter(filter, FilterReadContext::SnapshotOrEvent, mode)
        }
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
        // `destination` refines the same event-ledger read (the moved object's
        // current zone) — no new axis beyond the `event: true` already set.
        AbilityCondition::ZoneChangedThisWay {
            filter,
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
    // CR 732.2a firewall census discipline. `LiveBoardCensus`: this CALL
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
        TargetFilter::SourceController => Axes::NONE,
        TargetFilter::Opponent => Axes::NONE,
        TargetFilter::SelfRef => Axes::NONE,
        // CR 201.5a: a source-relative object ref (the granting object), like
        // SelfRef — no event/sibling/projected resource axis.
        TargetFilter::GrantingObject => Axes::NONE,
        // CR 608.2c: source-relative object ref (concretized to SpecificObject),
        // like SelfRef — no event/sibling/projected resource axis.
        TargetFilter::OriginalSource => Axes::NONE,
        TargetFilter::SourceOrPaired => Axes::NONE,
        // CR 106.1 / CR 119 / CR 122.1: a Typed target filter reads a PROJECTED player
        // resource ONLY via a property/controller that references one (authority:
        // `project_out_resources`). Pure type/controller predicates read none;
        // `event`/`sibling` stay CONSERVATIVE, only the projected axis is refined.
        //
        // ⛔ INVARIANT (CR 732.2a firewall soundness): this arm is the SOLE
        // `sibling: true` source inside `scan_target_filter`. A board-AGGREGATE
        // caller (a color/type-from-board mana metric, a `scan_quantity_ref`
        // `ObjectCount`, an `IsPresent` static condition) MUST self-assert its OWN
        // `sibling: true` literal and only THEN `.or(scan_target_filter(..))` — it
        // must NOT delegate its board-read signal to this `Typed` arm. Two reasons:
        // (a) a non-`Typed` board filter would be missed even today; (b) a future
        // `sibling: mode == Conservative` relaxation of this arm would silently turn
        // every delegating aggregate into a false certificate.
        TargetFilter::Typed(tf) => {
            // CR 732.2a: a mode-divergent arm. `event` stays unconditionally true
            // (byte-preserved).
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
        TargetFilter::LastRevealed | TargetFilter::LastZoneChanged => Axes::NONE,
        // CR 701.47c: per-resolution local (the Army amass just touched) — no
        // event/sibling/projected axis, mirroring `ObjectScope::AmassedArmy`.
        TargetFilter::AmassedArmy => Axes::NONE,
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
        // CR 102.1: the nested player predicate can itself read projected state
        // (`ControlsCount` over a whole `TargetFilter`, `PlayerAttribute` over a
        // `QuantityExpr`), so RECURSE rather than reporting `Axes::NONE` —
        // mirroring the object-axis `FilterProp::ControllerMatches` arm.
        TargetFilter::PlayerMatching { player } => scan_player_filter(player, mode),
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
        // CR 120.1: per-iteration batch source — a resolution-filtered object
        // with no event/sibling axis (mirrors Source/Target).
        ObjectScope::BatchSource => Axes::NONE,
        ObjectScope::EventTarget => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
    }
}

/// CR 732.2a: the read-axis surface of a `TriggerDefinition`, wherever the walk meets one —
/// the trigger-side twin of [`ability_definition_axes`], and this file's single authority for
/// one, so a payload site delegates here rather than re-deriving a matcher discipline. Its
/// carriers: a trigger GRANTED by a static ability (CR 604.1, via
/// `ContinuousModification::GrantTrigger`), and the firing condition of a DELAYED triggered
/// ability created by an effect (CR 603.7, via `scan_delayed_trigger_condition`).
///
/// FOR THE GRANTED CARRIER, the object-growth firewall already scans an INSTALLED
/// trigger's `condition` + `execute` on the same layer-flushed frame (`analysis::resource`
/// `fire_time_conditions_read_growing_class_scoped`), so the blanket `Axes::CONSERVATIVE`
/// this replaces was a redundant SECOND
/// veto on content already read; a DELAYED trigger is attached to no object, is never reached
/// by that scan, and the descent there is a FIRST read. Descending is what lets the firewall
/// NOT over-veto a granted trigger whose body reads nothing (Bello, Bard of the Brambles'
/// "Whenever this creature deals combat damage to a player, draw a card") — the same reason the
/// sibling `GrantAbility` arm descends.
///
/// **EXHAUSTIVE destructure with NO `..` rest pattern**, the discipline
/// [`ability_definition_axes`] and [`resolved_ability_axes`] use: a FUTURE
/// `TriggerDefinition` field fails to compile here until it is classified as
/// scanned or read-free. Every `_` binding below carries its justification.
fn scan_trigger_definition(t: &TriggerDefinition, mode: ScanMode) -> Axes {
    let TriggerDefinition {
        // ---- read-bearing: scanned into `acc` below ----
        execute,
        condition,
        // CR 603.2 + CR 603.2h: a fire-count / fire-window gate, which can carry
        // its own spell filter. NOT an event matcher, and no firewall surface
        // reads it (`analysis::resource` reads only `condition` and `execute`), so
        // this walker is its ONLY classifier ⇒ scanned, never read-free.
        constraint,
        // CR 118.12: a resolution-time "unless [player] pays [cost]" payload, NOT
        // an event matcher. The same `UnlessPayModifier` type that
        // `ability_definition_axes` already binds fail-closed rather than ignoring.
        unless_pay,

        // ---- read-bearing: EVENT-MATCHER filters. CR 603.2 — "whenever a game
        //      event or game state MATCHES a triggered ability's trigger event,
        //      that ability automatically triggers". Scanned, not assumed inert:
        //      both carriers that reach here ship matchers carrying real filters,
        //      so leaving these `_` would let a board-reading matcher pass the
        //      firewall unread.
        //
        //      `SnapshotOrEvent`, NOT the `LiveBoardCensus` default: a matcher
        //      tests the ONE triggering event's object/player against a filter and
        //      counts no live battlefield membership, so this CALL SITE injects no
        //      `sibling` of its own — the same discipline as `payer` below. A
        //      matcher whose filter DOES have a board-reading component still
        //      reports `sibling` through `typed_filter_axes` ->
        //      `scan_filter_prop`'s census recursion, so declining the call-site
        //      injection is not a relaxation hole.
        valid_card,
        valid_target,
        valid_subject_player,
        valid_source,
        zone_change_clauses,

        // ---- read-free: the Room-half (door) STAMP — a matcher key like the
        //      filters above, at its most inert end. `RoomDoor` is a fieldless two-variant
        //      discriminator (`game_object`), so unlike `valid_card` it cannot even
        //      express a filter: no payload position reaches a `TargetFilter` or a
        //      `QuantityExpr`, and it opens no traversal-closure hole. It is fixed per
        //      definition — the only writes are the once-claimed stamps in
        //      `GameObject::install_room_door_text`, so no sibling resolving first can
        //      move it. Both readers GATE which printed text acts, never a
        //      resolution-time value: CR 709.5h (an unlock ability triggers for ITS
        //      half) compares the stamp to the event's own `door` tag, and CR 709.5 (a
        //      locked half has no rules text) drops a stamped trigger while THAT
        //      object's own designation is absent. Neither verdict varies with the
        //      growing class's size, so a door gate can only NARROW which triggers
        //      reach the firewall.
        room_door: _,

        // ---- read-free: pure event SHAPE (CR 603.2). The only `TriggerMode`
        //      variants carrying a payload hold an ability tag, a planeswalk role,
        //      an ability-lifecycle point, or an `Unknown(String)` fallback; none
        //      reaches a `TargetFilter` or a `QuantityExpr`.
        //      ⚠ `mode: _` is load-bearing for COMPILATION as well as for
        //      classification: binding this field by name would shadow the
        //      `mode: ScanMode` parameter that every delegation below threads.
        mode: _,

        // ---- read-free: zone / phase / flag / literal-threshold metadata. No payload
        //      position of any field below reaches a `TargetFilter` or a
        //      `QuantityExpr`; every field that does is handled above.
        origin: _,
        origin_zones: _,
        destination: _,
        destination_constraint: _,
        trigger_zones: _,
        phase: _,
        optional: _,
        damage_kind: _,
        secondary: _,
        spell_cast_origin: _,
        description: _,
        counter_filter: _,
        saga_chapter: _,
        batched: _,
        die_sides: _,
        expend_threshold: _,
        attack_target_filter: _,
        player_actions: _,
        scry_bottom_count: _,
        damage_amount: _,
        life_amount: _,
        coin_flip_result: _,
        die_result: _,
        taps_for_mana_produced: _,
        mana_ability_produced: _,
        clash_result: _,
    } = t;

    let mut acc = Axes::NONE;
    if let Some(exec) = execute {
        acc = acc.or(ability_definition_axes(exec, mode));
    }
    if let Some(cond) = condition {
        acc = acc.or(scan_trigger_condition(cond, mode));
    }
    if let Some(c) = constraint {
        acc = acc.or(scan_trigger_constraint(c, mode));
    }
    if let Some(UnlessPayModifier { cost, payer }) = unless_pay {
        acc = acc.or(scan_ability_cost(cost, mode));
        // CR 118.12: `payer` names WHO PAYS — a player selector, not a board
        // census; nothing here counts objects. Same field class as
        // `target_chooser` and `optional_player`, which `ability_definition_axes`
        // already routes at `SnapshotOrEvent`. Declining the census default here is
        // NOT a relaxation hole — a payer that DOES
        // read the board still reports `sibling` through `typed_filter_axes` ->
        // `scan_filter_prop`'s census recursion; `SnapshotOrEvent` declines only
        // this call site's OWN injection, and `cost` stays scanned regardless.
        acc = acc.or(scan_target_filter(
            payer,
            FilterReadContext::SnapshotOrEvent,
            mode,
        ));
    }
    // CR 603.2 event matchers — context rationale in the destructure above.
    let mut matcher = Axes::NONE;
    for f in [valid_card, valid_target, valid_subject_player, valid_source]
        .into_iter()
        .flatten()
    {
        matcher = matcher.or(scan_target_filter(
            f,
            FilterReadContext::SnapshotOrEvent,
            mode,
        ));
    }
    // CR 603.6: each disjunctive clause carries its own card matcher. EXHAUSTIVE
    // destructure with no `..`, like the parent: a future `ZoneChangeClause` field
    // is `E0027` here until classified.
    for ZoneChangeClause {
        valid_card: clause_card,
        origin: _,                 // CR 603.6c source-zone constraint, no filter payload
        destination: _,            // Zone tag
        destination_constraint: _, // CR 700.4 "dies" predicate for LTB forms, no filter
    } in zone_change_clauses
    {
        if let Some(f) = clause_card {
            matcher = matcher.or(scan_target_filter(
                f,
                FilterReadContext::SnapshotOrEvent,
                mode,
            ));
        }
    }
    // The `event` axis does NOT propagate off a matcher. CR 603.2 vs CR 603.4: a
    // matcher SELECTS which event fires the ability; the `event` axis records a
    // resolution-time read of that event's characteristics, and its only consumer
    // is the distinct-event ordering term. Two copies of one granted trigger see
    // the same event through the same matcher, so a matcher can never make their
    // group order-relevant — and the CR 732.2a firewall reads `sibling`/`projected`
    // only. Those two DO propagate: they are the reads this descent exists to catch.
    acc = acc.or(Axes {
        event: false,
        ..matcher
    });
    acc
}

/// CR 603.2 + CR 603.2h: read-axes of a trigger's fire-count / fire-window
/// constraint.
///
/// TOTALITY, TWO LEVELS — both load-bearing, and the second is STRICTER than this
/// file's own convention, so do not "tidy" either away:
///  1. NO `_` wildcard arm ⇒ a new `TriggerConstraint` variant is `E0004` until
///     classified (the discipline `scan_filter_prop` documents).
///  2. NO `..` field rest in ANY arm ⇒ a new field on an EXISTING variant is
///     `E0027` until classified. The sibling scanners deliberately do NOT do this
///     — `scan_filter_prop` writes `FilterProp::Counters { count, .. }` — so
///     rewriting this as `NthSpellThisTurn { filter, .. }` would read as a
///     consistency fix while silently deleting level 2. A `filter` field added to
///     a SECOND `TriggerConstraint` variant is exactly how this walker would go
///     stale, and level 2 is the only thing that would catch it.
fn scan_trigger_constraint(x: &TriggerConstraint, mode: ScanMode) -> Axes {
    match x {
        // CR 603.2: a filtered spell-count gate — "your second [qualifier] spell
        // each turn" counts only spells matching `filter`. A newly-added
        // filter call site is `LiveBoardCensus` until PROVEN snapshot/event, and
        // the same-file precedent for count-gate filters is unanimous
        // (`QuantityRef::ObjectCount`, `TriggerCondition::ControlsType`,
        // `TriggerCondition::ControlCount` all route here). Deliberately NOT
        // `payer`'s `SnapshotOrEvent` answer: that is a player selector, this is a
        // count gate — adjacency is not an argument.
        TriggerConstraint::NthSpellThisTurn {
            filter,
            n: _,
            comparator: _,
        } => filter.as_ref().map_or(Axes::NONE, |f| {
            scan_target_filter(f, FilterReadContext::LiveBoardCensus, mode)
        }),
        // CR 109.5 + CR 603.2: the gate reads the triggering event's cause
        // relative to the trigger's controller — routed through the shared
        // controller-ref classifier rather than assumed read-free.
        TriggerConstraint::EventSourceControlledBy { controller } => {
            scan_controller_ref(controller)
        }
        // CR 603.2h fire-count gates, turn/phase windows, and class levels:
        // literal thresholds and per-turn counters only. No filter, no board
        // aggregate, no player resource.
        TriggerConstraint::OncePerTurn
        | TriggerConstraint::OncePerGame
        | TriggerConstraint::OnlyDuringYourTurn
        | TriggerConstraint::NthDrawThisTurn { n: _ }
        | TriggerConstraint::OnlyDuringOpponentsTurn
        | TriggerConstraint::OnlyDuringYourMainPhase
        | TriggerConstraint::AtClassLevel { level: _ }
        | TriggerConstraint::MaxTimesPerTurn { max: _ }
        | TriggerConstraint::OncePerOpponentPerTurn => Axes::NONE,
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
        TriggerCondition::SourceEnteredThisTurn | TriggerCondition::SourceAttackedThisCombat => {
            Axes {
                event: false,
                sibling: false,
                projected: true,
            }
        }
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
        // CR 701.54a + CR 701.54d: the condition reads the triggering
        // temptation's immutable chosen bearer.
        TriggerCondition::ChoseOtherRingBearer => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        // Same event-bound read as `ChoseOtherRingBearer`: the chooser and
        // the chosen bearer live on the triggering temptation event.
        TriggerCondition::ChoseRingBearer => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
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
        // CR 725.1: the monarch predicate itself reads no axis; its subject
        // scope is classified per-axis by the shared `PlayerScope` classifier,
        // mirroring `WasStartingPlayer { controller }`'s delegation to
        // `scan_controller_ref`.
        TriggerCondition::IsMonarch { player } => scan_player_scope(player),
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
        TriggerCondition::HasEnduringStory => Axes::NONE,
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
        // CR 903.3d: live battlefield census — same self-asserted board read as the
        // `AbilityCondition` / `StaticCondition` mirrors of this printed clause.
        TriggerCondition::ControlsCommander { ownership: _ } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
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

/// CR 603.7 + CR 732.2a: does a delayed trigger's FIRING CONDITION read a mutable
/// board aggregate?
///
/// A member of this file's condition-scanner family ([`scan_ability_condition`],
/// [`scan_trigger_condition`], [`scan_static_condition`],
/// [`scan_replacement_condition`]) — this was the condition type that had none, which
/// is why its carrier was a blanket. Each arm delegates to the authority that owns its
/// payload type. EXHAUSTIVE, NO `_` wildcard and NO `..` field rest, so a NEW
/// `DelayedTriggerCondition` variant or field is a compile error rather than a silent
/// read-free classification.
fn scan_delayed_trigger_condition(c: &DelayedTriggerCondition, mode: ScanMode) -> Axes {
    match c {
        // A phase coordinate. No payload position reaches a `TargetFilter` or a
        // `QuantityExpr`, so there is nothing to walk.
        DelayedTriggerCondition::AtNextPhase { phase: _ } => Axes::NONE,
        // The same coordinate plus a `PlayerId`, a `TurnGate` turn-floor, and a
        // `DelayedTriggerPlayerBinding` (which player `player` resolves to at
        // creation) — a named player, a turn number, and a binding tag. None
        // reaches a filter or a quantity.
        DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: _,
            player: _,
            gate: _,
            binding: _,
        } => Axes::NONE,
        // CR 603.7c: a delayed triggered ability that refers to a particular object.
        // `object_id` is already resolved, so there is no filter to walk and no
        // population whose size a growing class could move.
        DelayedTriggerCondition::WhenLeavesPlay { object_id: _ } => Axes::NONE,
        // Fails CLOSED. The payload is a bare `TargetFilter` with NO owning authority
        // to delegate to — the arms below have one, and `effect_target_ctx` is not it,
        // because it classifies EFFECT targets and a delayed-trigger matcher is not
        // one. Replicating a matcher discipline inline would mint a second, unowned
        // copy of it. `FilterReadContext`'s census default is the safe direction
        // for a contested new call site: over-veto, never a false offer.
        DelayedTriggerCondition::WhenDies { filter }
        | DelayedTriggerCondition::WhenLeavesPlayFiltered { filter }
        | DelayedTriggerCondition::WhenEntersBattlefield { filter }
        | DelayedTriggerCondition::WhenDiesOrExiled { filter } => {
            scan_target_filter(filter, FilterReadContext::LiveBoardCensus, mode)
        }
        // CR 603.2: the payload is a whole trigger EVENT MATCHER, and this file's
        // single authority for a `TriggerDefinition` is [`scan_trigger_definition`] —
        // never `scan_target_filter`, which would apply a filter discipline to a
        // many-field structure. `expiry` is the CR 603.7b stated-duration axis, not a
        // read.
        DelayedTriggerCondition::WheneverEvent { trigger, expiry: _ } => {
            scan_trigger_definition(trigger, mode)
        }
        // CR 603.2: TWO matchers of equal authority, and `or_trigger` is the drop
        // point — a `{ trigger, .. }` destructure compiles, passes every
        // variant-coverage test, and silently classifies only the first. `lifetime` is
        // the CR 603.7b stated-duration axis, not a read.
        DelayedTriggerCondition::WhenNextEvent {
            trigger,
            or_trigger,
            lifetime: _,
        } => {
            let mut acc = scan_trigger_definition(trigger, mode);
            if let Some(t) = or_trigger {
                acc = acc.or(scan_trigger_definition(t, mode));
            }
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
        // CR 611.2b: payload-free, exactly like `UntilHostLeavesPlay` — the
        // controller it is measured against lives on the carrier.
        Duration::WhileControllingHost => Axes::NONE,
        // CR 611.2b + CR 702.26f: presence-bound sibling, likewise payload-free.
        Duration::WhileHostOnBattlefield => Axes::NONE,
        Duration::UntilSourceExilesAnotherCard => Axes::NONE,
        Duration::UntilOpponentBecomesMonarch => Axes::NONE,
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
        // CR 725.1: see the `TriggerCondition::IsMonarch` arm above — the
        // subject scope is classified through the shared `PlayerScope` walker.
        StaticCondition::IsMonarch { player } => scan_player_scope(player),
        StaticCondition::IsInitiative => Axes::NONE,
        StaticCondition::NoMonarch => Axes::NONE,
        StaticCondition::HasCityBlessing => Axes::NONE,
        StaticCondition::HasEnduringStory => Axes::NONE,
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
        // CR 508.6: turn-history projection over the cleanup-time attack snapshot;
        // mirrors `SpellCastWithVariantThisTurn` (projected, not event/sibling).
        StaticCondition::AnyPlayerAttackedYouLastTurn => Axes {
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
        StaticCondition::DuringOpponentsTurn => Axes::NONE,
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
        // CR 903.3d: live battlefield census — same self-asserted board read as the
        // `AbilityCondition` / `TriggerCondition` mirrors of this printed clause.
        StaticCondition::ControlsCommander { ownership: _ } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
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
        | FilterProp::SameNameAsExiledBySource
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
        // CR 603.3b + CR 608.2c: the membership set is published by a PRECEDING
        // SIBLING effect in the same chain, and the per-member filter reads live
        // board state for members still on the battlefield — both are
        // sibling-mutable. A newly-added filter site is classified
        // `LiveBoardCensus` (fail-closed), matching `ControlsCount`.
        PlayerFilter::TrackedSetPossessor {
            filter,
            relation: _,
            possession: _,
            caused_by: _,
        } => Axes {
            event: false,
            sibling: true,
            projected: false,
        }
        .or(scan_target_filter(
            filter,
            FilterReadContext::LiveBoardCensus,
            mode,
        )),
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
        // CR 702.37b: reads the resolution-local turn-up payment fact ("if
        // its megamorph cost was paid to turn it face up") — an event-scoped
        // signal, no board census and no projected resource.
        ReplacementCondition::TurnUpCostSourcePaid { source: _ } => Axes {
            event: true,
            sibling: false,
            projected: false,
        },
        // CR 614.1d: an "enters tapped unless you control a [subtype]" self-entry
        // replacement. Its evaluator censuses the live battlefield — other permanents
        // this controller controls carrying a listed subtype — and reads no
        // triggering-event characteristic and no projected player resource, so the
        // verdict below is the narrow census literal, not `Axes::CONSERVATIVE`.
        //
        // The literal is inline rather than delegated because the payload is a list of
        // subtype names, not a `TargetFilter`: there is nothing to hand to
        // `scan_target_filter`, and inspecting it to relax the axis would re-open a
        // census the evaluator still runs.
        //
        // CR 732.2a: no disjointness arm matches this variant, so the only def the
        // replacement-condition accessor spares is one `replacement_is_spent_self_entry`
        // skips whole — an unblinked `SelfRef` entry on the battlefield.
        ReplacementCondition::UnlessControlsSubtype { subtypes: _ } => Axes {
            event: false,
            sibling: true,
            projected: false,
        },
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
        // CR 611.2: a frozen literal id — reads no event, sibling, or projected
        // resource.
        PlayerScope::SpecificPlayer { .. } => Axes::NONE,
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
        // CR 109.4 + CR 611.2: a frozen literal id — reads no event, sibling, or
        // projected resource.
        ControllerRef::SpecificPlayer { .. } => Axes::NONE,
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
/// distinct-event auto-resolve gate — the Rubblebelt Rioters / Orcish
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
// (`analysis::resource::loop_states_cover_modulo_object_growth`).
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
/// fails to compile until classified.
///
/// `cost` is bound read-free because this walker classifies RESOLUTION-time reads
/// and an activation cost is not one — CR 601.2f / CR 602.5a place a cost's read at
/// the moment it is PAID. The object-growth cost surface is a separate scan,
/// `analysis::resource::cost_surface_references_growing_class`, whose reach is
/// narrower than this walker's: it descends an object's `abilities` cost tree
/// (`cost` / `sub_ability` / `else_ability` / `mode_abilities`), so it does not see
/// a def handed to this walker through [`scan_trigger_definition`] or the
/// `Effect::CreateDelayedTrigger` arm. Those two carriers, not this binding, are
/// where a trigger-body cost surface has to be argued.
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
        // ---- read-free: cost is a payment-time read (see the doc above),
        //      announce-time metadata, flags, and tags — none express a
        //      resolution-time dynamic read ----
        kind: _,
        cost: _,
        description: _,
        target_prompt: _,
        activation_restrictions: _,
        // Payment-time only; it cannot create a resolution-time dependency.
        activation_mana_payment_restriction: _,
        activator_filter: _,
        activation_zone: _,
        ability_tag: _,
        optional_targeting: _,
        optional: _,
        optional_player,
        optional_for: _,
        target_choice_timing: _,
        min_x_value: _,
        cant_be_copied: _,
        forward_result: _,
        target_selection_mode: _,
        sub_link: _,
        iteration_kind_binding: _,
        sibling_condition: _,
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
    if let Some(player) = optional_player {
        acc = acc.or(scan_target_filter(
            player,
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
/// `obj.abilities` def regardless of `kind`, granted-ability bodies, and the
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

/// Axis 2 on any cost surface: EXHAUSTIVE `AbilityCost` match,
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
        | AbilityCost::GetPlayerCounters { .. }
        | AbilityCost::Unimplemented { .. } => Axes::NONE,
    }
}

/// The cost-KEYWORD family. Does casting or activating an object that
/// carries `kw` incur a cost whose MAGNITUDE or PAYABILITY is a function of a
/// battlefield/graveyard object quantity — i.e. the cost either (a) scales down by a
/// board/graveyard count, or (b) taps/sacrifices/exiles a member of a board or
/// graveyard object class? Such an IMPLICIT (keyword-driven) cost reads the inert
/// growth set |G| and breaks the fixed-cost extrapolation the object-growth cover
/// relies on (CR 732.2a: a cast-affordability the `ResourceVector`
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
/// ETB/triggered mechanic (whose board reads, if any, are caught by the
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
        | Keyword::Storied
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
        | Keyword::Devour { .. }
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

/// The granted-keyword cost family. A runtime-granted cost keyword
/// (`ContinuousModification::AddKeyword`) or a granted keyword whose cost is
/// derived from board state (`AddKeywordWithDerivedCost`) reaches the same
/// affordability hole as a printed one. Every other modification is not a
/// cost-keyword grant (read-free on THIS axis; its board reads, if any, are caught
/// by the effect-body firewall).
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
// These are the walkers that make the `Effect::Token`/`Effect::Mana` blankets
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
/// [`keyword_cost_reads_growing_class`] (the tap/sacrifice/exile/scale keywords),
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
        | Keyword::Flashback(_)
        | Keyword::Emerge(_) => Axes::CONSERVATIVE,
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
        | Keyword::Storied
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
        | Keyword::Devour { .. }
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
/// distinct sibling-read paths: a COUNT-DRIVEN metric's board read (if any)
/// lives entirely inside its `count` (self-guarded by `scan_quantity_ref`'s
/// `ObjectCount` arm), while a color/type-FROM-BOARD aggregate must self-assert
/// its OWN `sibling:true` (see the invariant at `scan_target_filter`'s `Typed`
/// arm). EXHAUSTIVE over every variant, NO `_` wildcard.
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
        // `NotedManaSpent` is mutable per-object state written by a companion
        // `Effect::NoteManaSpent`, so sibling activations can affect its value.
        ManaProduction::NotedType { count } => Axes {
            event: false,
            sibling: true,
            projected: false,
        }
        .or(scan_quantity_expr(count, mode)),
        // SCOPED-OBJECT (Omnath, Locus of All): a SINGLE scoped object's colors,
        // NOT a board aggregate — the scope's own read surface is the sole sibling
        // source (CR 202.2c). NO own sibling literal.
        ManaProduction::AnyCombinationOfObjectColors { count, scope } => {
            scan_quantity_expr(count, mode).or(scan_object_scope(scope))
        }
        // ⛔ BOARD-AGGREGATE (color/type-from-board): self-assert OWN `sibling:true`
        // (mirror `scan_quantity_ref`; must NOT delegate the board read to the
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
/// over every `ContinuousModification` variant, NO `_` wildcard — a new variant
/// fails to compile until classified. `mode` is threaded to both granted-body
/// descents (`GrantAbility` / `GrantTrigger`) so a token body inside a grant is
/// classified in the same mode. The AST is finite and acyclic, so the mutual
/// recursion terminates.
fn scan_continuous_modification(m: &ContinuousModification, mode: ScanMode) -> Axes {
    match m {
        // descend the dynamic P/T / dynamic-keyword / enter-counter QuantityExpr
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
        // descend the granted keyword (routes through the same authority)
        ContinuousModification::AddKeyword { keyword }
        | ContinuousModification::RemoveKeyword { keyword } => scan_keyword(keyword, mode),
        // descend a granted ability body (GrantAbility). Presence of Gond's aura
        // grants a `{T}: Create ...` activated ability whose token body reads
        // nothing sibling — descending is what lets the firewall NOT over-veto it.
        ContinuousModification::GrantAbility { definition } => {
            ability_definition_axes(definition, mode)
        }
        // descend a granted TRIGGER body (GrantTrigger) — the same move, and the
        // same reason, as the `GrantAbility` arm directly above. See
        // `scan_trigger_definition` for the per-field scanned/read-free split.
        ContinuousModification::GrantTrigger { trigger } => scan_trigger_definition(trigger, mode),
        // fail-closed CONSERVATIVE: inner payloads with no walker.
        ContinuousModification::CopyValues { .. }
        // CR 707.2c (Metamorphic Alteration): the copy-marker stands in for a
        // copy that grants the donor's whole ability set — fail-closed alongside
        // its `CopyValues` sibling (the real grant is the installed TCE).
        | ContinuousModification::CopyChosen
        // A granted object-hosted replacement's `ReplacementDefinition` execute
        // is outside the scanner's traversal closure — fail-closed CONSERVATIVE,
        // same class as GrantStaticAbility (`GrantTrigger` left this group when
        // `scan_trigger_definition` gave its payload a walker).
        | ContinuousModification::GrantReplacement { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        // Sakashima's copy-layer "retain this object's own abilities"
        // — same class as the RetainPrinted* siblings (no inner walker) ⇒ fail-closed.
        | ContinuousModification::RetainAllOtherAbilitiesFromSource => Axes::CONSERVATIVE,
        // read-free: static structural mods (name/type/color/anthem/chosen-
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

/// LoopFirewall-mode growing class (`sibling` ∨ `projected`) on a def-level
/// `AbilityDefinition` (trigger `execute` bodies, every functioning `obj.abilities`
/// def, granted-ability bodies) — the CR 732.2a object-growth firewall's DESCENDING
/// body scan.
///
/// Both axes, and what `Conservative` consumers ask instead: see
/// [`Axes::reads_growing_class`].
pub(crate) fn ability_definition_reads_growing_class_for_loop(def: &AbilityDefinition) -> bool {
    ability_definition_axes(def, ScanMode::LoopFirewall).reads_growing_class()
}

/// CR 732.2a growing class (`sibling` ∨ `projected`) on ONE effect-TARGET filter,
/// under that effect's OWN census discipline. `target` MUST be a target-filter field
/// of `effect`: the `FilterReadContext` is derived from `effect` by
/// [`effect_target_ctx`], the same derivation `scan_effect` makes for its own
/// [`scan_target_filter`] calls, so a re-grouping of that effect moves this answer
/// with it. Both axes, and what `Conservative` consumers ask instead: see
/// [`Axes::reads_growing_class`].
///
/// `pub(crate)` for ONE reason: `analysis::resource`'s relief arm
/// `pump_aggregate_provably_excludes_class` must prove `Effect::Pump`'s target
/// contributes no growing-class read before relieving that def's veto — a veto the
/// aggregate `PtValue` half carries, since [`scan_quantity_ref`] marks it `sibling`
/// before walking the filter. Its sibling arms state this as a `target: None`
/// PATTERN, which `Effect::Pump` cannot: its `target` is not an `Option<_>`.
pub(crate) fn effect_target_reads_growing_class_for_loop(
    effect: &Effect,
    target: &TargetFilter,
) -> bool {
    // The doc's "`target` MUST be a target-filter field of `effect`" was a request with
    // nothing binding the two arguments: a caller passing an unrelated filter would get a
    // verdict computed under a DIFFERENT effect's census discipline, silently and with no
    // diagnostic. `Effect::target_filter()` is the authority for that relation and answers
    // `Some` for `Effect::Pump`, which is this function's whole reason for being
    // `pub(crate)`. Value equality, not pointer identity — a caller may legitimately hold a
    // clone of the field.
    debug_assert!(
        effect.target_filter() == Some(target),
        "`effect_target_reads_growing_class_for_loop` derives its `FilterReadContext` from \
         `effect`, so `target` must BE that effect's target filter — otherwise the verdict is \
         computed under a census discipline belonging to a different effect"
    );
    scan_target_filter(
        target,
        effect_target_ctx(effect, ScanMode::LoopFirewall),
        ScanMode::LoopFirewall,
    )
    .reads_growing_class()
}

/// CR 613.1 + CR 732.2a: does a live continuous modification READ a mutable board
/// aggregate (axis-2 `sibling`)? Consumed by
/// `analysis::resource::fire_time_conditions_read_growing_class_scoped`'s live
/// continuous-modification descent.
pub(crate) fn continuous_modification_reads_sibling_mutable(m: &ContinuousModification) -> bool {
    scan_continuous_modification(m, ScanMode::LoopFirewall).sibling
}

/// CR 106.1 / CR 119 / CR 122.1 + CR 732.2a: does a live continuous modification
/// READ a projected player resource (axis-3 `projected`)? Load-bearing: the
/// projected-resource firewall has NO modification scan, so that descent is
/// the sole guard against a projected-reading modification (a
/// `SetDynamicPower{Ref(LifeTotal)}` anthem).
pub(crate) fn continuous_modification_reads_projected_resource(m: &ContinuousModification) -> bool {
    scan_continuous_modification(m, ScanMode::LoopFirewall).projected
}

/// CR 732.2a: the census discipline for `scan_effect`'s effect-TARGET filter reads.
/// INVERTED default — every effect-target read is `LiveBoardCensus` (veto = safe)
/// unless the effect is in the pinned proven-inert exception set, so an unclassified
/// or future cardinality-driving `Effect` lands fail-CLOSED (missed offer, never a
/// false one). EXHAUSTIVE `match e`, NO `_` wildcard, and no `_ => SnapshotOrEvent`
/// anywhere in this path: a NEW `Effect` variant fails to compile until placed in
/// one of the two arms. The exception set is `{SetTapState}` ONLY (obligation (ii):
/// tap/untap of an INERT grown token feeds no drivability, and the stable host's tap
/// state is part of the certified recurrence via `board_covers`); the two damage
/// aggregates fall to census automatically because their `.sources` cardinality
/// DRIVES escalating player damage.
///
/// Mode-gate: under `Conservative` the decision does NOT exist — effect targets pass
/// a fixed `SnapshotOrEvent`, BYTE-IDENTICAL to the pre-descent scan.
fn effect_target_ctx(e: &Effect, mode: ScanMode) -> FilterReadContext {
    // Mode-gate the ROUTING. Under `Conservative` effect targets pass a
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
        // sibling veto catches it. Pinned EXACTLY by `census_tag_set_is_exactly_enumerated`.
        // Defense-in-depth: PumpAll/DamageEachPlayer/ChangeZoneAll are census-
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
        // CR 701.60a: Suspect/Unsuspect scope:All is a mass-population battlefield
        // read (`target_filter()`==None; `suspect.rs` enumerates `state.battlefield`,
        // "like DestroyAll") ⇒ census — its read SCALES with the growing class. scope:Single
        // is a single announced target (a2), relaxed in the single-object group below. The
        // two scopes are exhaustive for Suspect/Unsuspect (EffectScope = {Single, All}).
        // Fail-CLOSED: over-vetoes the Absolving Lammasu mass-unsuspect shortcut OFFER
        // (missed offer, never a false certificate).
        | Effect::Suspect { scope: EffectScope::All, .. }
        | Effect::Unsuspect { scope: EffectScope::All, .. }
        // CR 701.27a + CR 115.10a: mass Transform ("Transform all Humans", scope:All)
        // is a non-targeting battlefield-population read (`target_filter()`==None;
        // `transform_effect::resolve_all` enumerates `state.battlefield`, like
        // DestroyAll) ⇒ census — its read SCALES with the growing class. Unlike the
        // state-convergent SetTapState exception below, Transform WRITES ObjectPt and
        // swaps the object's abilities, so a grown token is NOT inert and the read can
        // escalate: `LiveBoardCensus`, never the Snapshot exception. scope:Single is a
        // single announced/anaphoric target (a2), relaxed in the single-object group
        // below. Exhaustive over EffectScope = {Single, All}.
        | Effect::Transform { scope: EffectScope::All, .. }
        // ── DUAL-MODE MASS-BATTLEFIELD RESOLVERS: each has a resolver mode that, when
        // the ability carries NO explicit object target, enumerates the battlefield (or
        // all phased-in/-out permanents) and applies the effect to EVERY matching
        // object — a MASS-POPULATION read that SCALES with the growing class, exactly
        // like the DestroyAll/PumpAll group above. NO static field discriminates the
        // announced-single mode from the mass mode (it is the resolution-time
        // `ability.targets.is_empty()` / `ParentTarget` branch), so the WHOLE variant
        // censuses: over-vetoing the bounded mode is the SAFE direction. Each
        // `scan_effect` arm routes its filter through this `target_ctx` (BecomeCopy is
        // `Axes::CONSERVATIVE` today, so its tag is defense-in-depth parity with
        // PumpAll/ChangeZoneAll). Pinned by `census_tag_set_is_exactly_enumerated`.
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
        //   CR 707.2c (Metamorphic Alteration): the copy target is chosen from a
        //     battlefield-reading filter pool; defense-in-depth parity with
        //     BecomeCopy (fail-closed census — over-vetoes the single-host shortcut).
        | Effect::ChoosePermanent { .. }
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
        //   CR 608.2d + CR 122.1: a typed counter-kind source domain enumerates
        //     every matching permanent at resolution and unions the kinds of
        //     counters on them, so the read scales with battlefield growth.
        | Effect::ChooseCounterKind { .. }
        //   CR 707.2 + CR 509.1g + CR 506.3e: `copy_token_blocking.rs` UNCONDITIONALLY
        //     enumerates
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
        // (`object_is_inert`) AND its tap flag is compared by
        // board_covers, so the read cannot escalate. NOT a general (b)-license — a specific
        // proven exception. Destructured no-`..` so a new field forces re-audit. Pinned by
        // `obligation_ii_census_exception_is_exactly_settapstate`.
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
        | Effect::RevealChosenNumbers { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::RemoveCounter { .. }
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
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::NoteManaSpent
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
        | Effect::ReproduceEventCounters { .. }
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
        // CR 701.27a: only the scope:Single Transform relaxes — a single announced or
        // anaphoric target (a2). scope:All is the mass battlefield read, census-tagged
        // above with the DestroyAll/Suspect{All} group.
        | Effect::Transform { scope: EffectScope::Single, .. }
        // CR 710.4: same single-target read context as `Transform` (always self-ref).
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        // CR 400.11: the filter reads the OPENED PACK's cards, never the board.
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        // CR 701.60a: only the scope:Single Suspect/Unsuspect relaxes — a single
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
        // Susan Foreman reorders the PLANAR DECK top (Planechase),
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
        | Effect::CompletePlayerAction { .. }
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

// CR 732.2a: census-completeness PARTITION — the INDEPENDENT oracle that
// cross-checks `effect_target_ctx`, closing the gap where a census-ROLE slot sits
// silently in that function's generic relax `|`-chain. EVERY `Effect` variant is
// classified EXHAUSTIVELY (NO wildcard) into `Census` (mass battlefield population,
// scales with growth => fail-closed) or `Relax(reason)`, so a new variant is a
// compile error until a human assigns its role, and
// `census_partition_agrees_with_effect_target_ctx` asserts the two functions'
// `Census` sets are byte-identical. `#[cfg(test)]` guard infrastructure, not runtime
// code. The discriminating property is BATTLEFIELD-MASS-POPULATION, NOT
// `target_filter()==None`: `Effect::UnattachAll` is `Some` yet census-ROLE, while
// `Dig`/`Seek`/`SearchOutsideGame`/`RevealHand` are `None` yet correctly RELAXED
// (library/hand/exile pools are DISJOINT from the battlefield growth class), so the
// classification is by scaling ROLE. The `Relax` reason sub-tags are documentation;
// only the `Census`/`Relax` boundary is guard-enforced.

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
/// independent, exhaustively-classified oracle.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CensusRole {
    /// Mass battlefield population read - enumerated over every matching object,
    /// scales with the growing class => fail-CLOSED census. EXACTLY the
    /// `effect_target_ctx` `LiveBoardCensus` members.
    Census,
    Relax(RelaxReason),
}

/// Exhaustive per-variant census-role classification (NO wildcard). A new
/// `Effect` variant is a compile error until placed in one of the arms below,
/// converting the silent-miss into a forced, reasoned decision for the whole
/// CLASS. Cross-checked against `effect_target_ctx` by
/// `census_partition_agrees_with_effect_target_ctx`.
#[cfg(test)]
fn effect_census_role(e: &Effect) -> CensusRole {
    match e {
        // -- CENSUS: verbatim mirror of `effect_target_ctx`'s LiveBoardCensus
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
        // -- DUAL-MODE MASS-BATTLEFIELD RESOLVERS: mirror of the
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
        // CR 707.2c (Metamorphic Alteration): parity with the `effect_target_ctx`
        // LiveBoardCensus member added above.
        | Effect::ChoosePermanent { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::MultiplyCounter { .. }
        // CR 608.2d + CR 122.1: a typed counter-kind source domain scans the
        // matching battlefield population and unions its counter kinds.
        | Effect::ChooseCounterKind { .. }
        // CR 707.2 + CR 509.1g: `copy_token_blocking.rs` creates one
        // token copy per matching attacker over an UNCONDITIONAL battlefield scan (grows
        // the board); unsound across CR 508.1 multi-combat loops. Mirror of the new
        // effect_target_ctx census member.
        | Effect::CopyTokenBlockingAttacker { .. }
        // CR 701.27a + CR 115.10a: mass Transform (scope:All) enumerates
        // `state.battlefield` (`transform_effect::resolve_all`) — a census read that
        // GROWS with the class. It WRITES ObjectPt + swaps abilities (NOT state-
        // convergent like SetTapState), so it is a true `Census`, never the SetTapState
        // relax exception. Parity with the effect_target_ctx LiveBoardCensus member.
        | Effect::Transform {
            scope: EffectScope::All,
            ..
        } => CensusRole::Census,

        // -- SetTapState (scope-DESTRUCTURED, exhaustive over EffectScope): scope:All is
        // the census-ROLE proven exception (TapAll/UntapAll - state-convergent/idempotent,
        // does not escalate over inert growth); scope:Single is an ordinary single announced
        // target. BOTH relax, so both AGREE with effect_target_ctx's scope-blind SetTapState
        // Snapshot arm (the sole dedicated census-role exception, pinned by
        // `obligation_ii_census_exception_is_exactly_settapstate`).
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
        // CR 400.11: reads a pack from OUTSIDE the game — no board population at all.
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::RevealUntil { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::Surveil { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
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
        | Effect::RevealChosenNumbers { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::RemoveCounter { .. }
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
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::NoteManaSpent
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
        | Effect::ReproduceEventCounters { .. }
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
        // CR 701.27a: scope:Single Transform reads only its single announced/anaphoric
        // target — not a board census. scope:All is census-tagged above.
        | Effect::Transform {
            scope: EffectScope::Single,
            ..
        }
        // CR 710.4: a flip reads only its own self-referential target — not a
        // board census, mirroring `Transform`'s single scope.
        | Effect::FlipPermanent { .. }
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
        // Susan Foreman reorders the PLANAR DECK top (Planechase),
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
        | Effect::CompletePlayerAction { .. }
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
/// compiler enforces that the two lists stay in lockstep — the static,
/// compile-time-exhaustive half of the determinism gate.
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
        // CR 400.11 + CR 701.9b: opening a booster pack draws the seeded RNG
        // twice — the shelf product and the pack's collation — and the cards it
        // produces determine what the controller may then take. Unpredictable at
        // pin time, so a loop body containing one is not a legal CR 732.2a
        // shortcut.
        | Effect::OpenBoosterPack { .. }
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
        | Effect::RevealChosenNumbers { .. }
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
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::NoteManaSpent
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
        // CR 707.2c: choosing a permanent draws on no game randomness.
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::ReproduceEventCounters { .. }
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
        // CR 710.4: flipping is deterministic — no RNG draw, mirroring `Transform`.
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
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
        | Effect::CompletePlayerAction { .. }
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
/// no-offer separately. The announce-time half of the determinism gate.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityKind, AggregateFunction, CastManaObjectScope, CastManaSpentMetric, Comparator,
        CostDerivation, DamageKindFilter, DelayedTriggerLifetime, DestinationConstraint,
        ManaContribution, OriginConstraint, ReciprocalZoneChoiceRole, ReplacementDefinition,
        StaticDefinition, TurnGate, WheneverEventExpiry, ZoneChangeClause,
        ZoneChoiceCandidateSource, ZoneChoiceChooser, ZoneOwner,
    };
    use crate::types::counter::CounterType;
    use crate::types::identifiers::ObjectId;
    use crate::types::keywords::CostBearingKeywordKind;
    use crate::types::mana::{ManaColor, ManaCost};
    use crate::types::player::{PlayerCounterKind, PlayerId};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn zone_choice_for_scan(
        zone: Zone,
        candidate_source: ZoneChoiceCandidateSource,
        reciprocal_role: Option<ReciprocalZoneChoiceRole>,
    ) -> Effect {
        Effect::ChooseFromZone {
            count: 1,
            zone,
            additional_zones: vec![],
            zone_owner: ZoneOwner::Controller,
            filter: None,
            chooser: ZoneChoiceChooser::Controller,
            candidate_source,
            reciprocal_role,
            up_to: false,
            selection: Default::default(),
            constraint: None,
        }
    }

    /// Candidate provenance and reciprocal role carry resolution-chain reads
    /// even though their direct graveyard/filter payloads are inert.
    #[test]
    fn zone_choice_provenance_and_reciprocal_reads_are_classified() {
        let axes = |effect| scan_effect(&effect, ScanMode::LoopFirewall);
        assert_eq!(
            {
                let x = axes(zone_choice_for_scan(
                    Zone::Graveyard,
                    ZoneChoiceCandidateSource::Direct,
                    None,
                ));
                (x.event, x.sibling, x.projected)
            },
            (false, false, false),
            "a direct graveyard scan has no growing-class read"
        );
        for (label, effect) in [
            (
                "tracked provenance",
                zone_choice_for_scan(Zone::Graveyard, ZoneChoiceCandidateSource::Tracked, None),
            ),
            (
                "legacy provenance",
                zone_choice_for_scan(Zone::Graveyard, ZoneChoiceCandidateSource::Legacy, None),
            ),
            (
                "reciprocal consumer",
                zone_choice_for_scan(
                    Zone::Graveyard,
                    ZoneChoiceCandidateSource::Direct,
                    Some(ReciprocalZoneChoiceRole::Consume),
                ),
            ),
        ] {
            let x = axes(effect);
            assert_eq!(
                (x.event, x.sibling, x.projected),
                (true, true, true),
                "{label} must fail closed because it consumes a resolution-chain set"
            );
        }
        let producer = axes(zone_choice_for_scan(
            Zone::Graveyard,
            ZoneChoiceCandidateSource::Direct,
            Some(ReciprocalZoneChoiceRole::Produce),
        ));
        assert_eq!(
            (producer.event, producer.sibling, producer.projected),
            (false, false, false),
            "a reciprocal producer publishes, but does not read, its fresh tracked set"
        );
        let battlefield = axes(zone_choice_for_scan(
            Zone::Battlefield,
            ZoneChoiceCandidateSource::Direct,
            None,
        ));
        assert_eq!(
            (
                battlefield.event,
                battlefield.sibling,
                battlefield.projected
            ),
            (true, true, false),
            "a direct battlefield candidate pool is a live board census"
        );
    }

    #[test]
    fn property_aggregate_source_scan_axes_are_exhaustive() {
        use crate::types::ability::{
            CardTypeSetSource, CountScope, ObjectProperty, PropertyAggregate, TrackedAnaphorSource,
            TurnJournalKind, ZoneRef,
        };

        let chain = CardTypeSetSource::TrackedSet {
            set: TrackedAnaphorSource::ChainSet,
            caused_by: None,
        };
        let journal = CardTypeSetSource::TurnJournal {
            journal: TurnJournalKind::SpellsCast,
            scope: CountScope::Controller,
            filter: None,
        };
        let event_filtered_journal = CardTypeSetSource::TurnJournal {
            journal: TurnJournalKind::SpellsCast,
            scope: CountScope::Controller,
            filter: Some(TargetFilter::TriggeringSource),
        };
        let rows = vec![
            (
                CardTypeSetSource::Objects {
                    filter: TargetFilter::Any,
                },
                Axes {
                    event: false,
                    sibling: true,
                    projected: false,
                },
            ),
            (
                CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::TriggeringBatch,
                    caused_by: None,
                },
                Axes {
                    event: true,
                    sibling: false,
                    projected: false,
                },
            ),
            (chain.clone(), Axes::NONE),
            (
                journal.clone(),
                Axes {
                    event: false,
                    sibling: false,
                    projected: true,
                },
            ),
            (
                event_filtered_journal,
                Axes {
                    event: true,
                    sibling: false,
                    projected: true,
                },
            ),
            (
                CardTypeSetSource::Zone {
                    zone: ZoneRef::Graveyard,
                    scope: CountScope::Controller,
                },
                Axes::CONSERVATIVE,
            ),
            (CardTypeSetSource::ExiledBySource, Axes::CONSERVATIVE),
            (
                CardTypeSetSource::any_of(vec![chain, journal]).unwrap(),
                Axes {
                    event: false,
                    sibling: false,
                    projected: true,
                },
            ),
        ];
        for (source, expected) in rows {
            let qty = QuantityRef::PropertyAggregate(
                PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::ManaValue,
                    source.clone(),
                )
                .unwrap(),
            );
            let actual = scan_quantity_ref(&qty, ScanMode::Conservative);
            assert_eq!(
                (actual.event, actual.sibling, actual.projected),
                (expected.event, expected.sibling, expected.projected),
                "{source:?}"
            );
        }
    }

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

    #[test]
    fn unassigned_distribution_unit_adds_no_dynamic_read_axis() {
        let base = fixed_drain();
        let mut divided = base.clone();
        divided.distribute = Some(crate::types::game_state::DistributionUnit::Life);

        let base_axes = resolved_ability_axes(&base, ScanMode::Conservative);
        let divided_axes = resolved_ability_axes(&divided, ScanMode::Conservative);
        assert_eq!(
            (base_axes.event, base_axes.sibling, base_axes.projected),
            (
                divided_axes.event,
                divided_axes.sibling,
                divided_axes.projected
            )
        );
    }

    // ---- the ScanMode split + descending object-growth firewall ----

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

    /// A vanilla token stays fail-closed CONSERVATIVE in `Conservative` mode.
    /// Revert-probe: make the Token arm descend unconditionally ⇒ `event` flips false.
    #[test]
    fn conservative_mode_token_axes_are_unchanged() {
        let axes = scan_effect(&vanilla_token(), ScanMode::Conservative);
        assert!(axes.event && axes.sibling && axes.projected);
    }

    /// Same for `Effect::Mana`.
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

    /// The CR 603.3b trigger-ordering gate is byte-identical for a token-bodied
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

    /// The same vanilla token DESCENDS to NONE in `LoopFirewall` (reads
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

    // ---- the `Effect::Pump` blanket becomes a mode-split descent ----

    /// Pyreswipe Hawk's REAL attack pump, parsed from the card's VERBATIM Oracle text
    /// (MTGJSON `AtomicCards.json`) — never a paraphrase and never a hand-built `Pump`,
    /// either of which can take a different parser branch than the card the relief exists
    /// for. Its `power` is a `PropertyAggregate` over `Objects{Typed{Artifact, You}}`, which
    /// `scan_quantity_ref` marks `sibling` before it even walks the filter — so this is the
    /// PAIRED POSITIVE that stops a blanket relief from satisfying row 25.
    ///
    /// Pinned by TRIGGER MODE, not by index: the card parses two triggers and an index pin
    /// would silently re-point if the parser reorders them.
    fn hawk_attack_pump() -> Effect {
        let parsed = crate::parser::parse_oracle_text(
            "Flying, haste\n\
             Whenever this creature attacks, it gets +X/+0 until end of turn, where X is \
             the greatest mana value among artifacts you control.\n\
             Whenever you expend 6, gain control of up to one target artifact for as long \
             as you control this creature. (You expend 6 as you spend your sixth total mana \
             to cast spells during a turn.)",
            "Pyreswipe Hawk",
            &[],
            &["Creature".to_string()],
            &["Elemental".to_string(), "Bird".to_string()],
        );
        let attacks = parsed
            .triggers
            .iter()
            .find(|t| t.mode == TriggerMode::Attacks)
            .expect(
                "fixture pin: Pyreswipe Hawk's Oracle text must parse to an `Attacks` \
                 trigger — the attack pump is the whole subject of these rows",
            );
        let effect = attacks
            .execute
            .as_deref()
            .expect("fixture pin: the `Attacks` trigger carries an execute body")
            .effect
            .as_ref()
            .clone();
        // VACUITY GUARD: if the parser ever stops producing the aggregate, every row below
        // would still be green while proving nothing about a board-reading pump.
        assert!(
            matches!(
                &effect,
                Effect::Pump {
                    power: PtValue::Quantity(QuantityExpr::Ref {
                        qty: QuantityRef::PropertyAggregate(_)
                    }),
                    ..
                }
            ),
            "fixture pin: the attack pump's `power` must be a `QuantityRef::PropertyAggregate` \
             (\"the greatest mana value among artifacts you control\"), else the paired \
             positive below reads nothing and row 25 is satisfiable by a blanket relief"
        );
        effect
    }

    /// A `Pump` with two `PtValue::Fixed` halves and a read-free target — the shape row 25
    /// relieves. Migrated in from arm (vi) of `analysis::resource`'s
    /// `pump_aggregate_gate_is_precise_and_fail_closed`, which could no longer construct it
    /// once `pump_firewall_fixture`'s reach guard stopped being reachable with a read-free
    /// def.
    fn read_free_pump(target: TargetFilter) -> Effect {
        Effect::Pump {
            power: PtValue::Fixed(2),
            toughness: PtValue::Fixed(2),
            target,
        }
    }

    /// **Row 25** — a trivially-invariant `Pump` stops reading the sibling axis under
    /// `ScanMode::LoopFirewall`, and an aggregate-bearing one does NOT.
    ///
    /// CR 608.2h, carried verbatim from the arm (vi) row this replaces: *a `Pump` with two
    /// `PtValue::Fixed` halves reads NOTHING, so per CR 608.2h its value is trivially
    /// invariant across the loop's growth and block (1b) must skip it. If this row goes red
    /// because someone narrowed the descent to require an aggregate, the narrowing is
    /// keeping a veto that is provably unnecessary — re-argue it, do not delete this row.*
    ///
    /// Both arms assert the EXACT axis triple, so neither a blanket relief nor a blanket
    /// veto satisfies this row.
    ///
    /// REVERT-PROBE: restore `Effect::Pump { .. } => Axes::CONSERVATIVE` (drop the
    /// `match mode` split) ⇒ the read-free arm reports `(true, true, true)` ⇒ **FAILS**.
    #[test]
    fn scan_effect_pump_descends_under_loop_firewall() {
        let inert = read_free_pump(TargetFilter::SelfRef);
        let axes = scan_effect(&inert, ScanMode::LoopFirewall);
        assert_eq!(
            (axes.event, axes.sibling, axes.projected),
            (false, false, false),
            "CR 732.2a: a `Pump` with two `PtValue::Fixed` halves and a `SelfRef` target \
             reads NOTHING — an AST property, since `PtValue::Fixed` is a literal, and \
             deliberately NOT cited to a rule, because the Comprehensive Rules have nothing \
             to say about an effect that requires no information. What CR 732.2a supplies \
             is why that matters: no later choice in the proposed sequence becomes \
             conditional on how far the loop has run, so the results stay predictable and \
             the firewall must not veto. If this row goes red because someone narrowed the \
             descent to require an aggregate, the narrowing is keeping a veto that is \
             provably unnecessary — re-argue it, do not delete this row"
        );

        // PAIRED POSITIVE, on the same shape: the real card body must KEEP its veto.
        let hawk = hawk_attack_pump();
        let hawk_axes = scan_effect(&hawk, ScanMode::LoopFirewall);
        assert_eq!(
            (hawk_axes.event, hawk_axes.sibling, hawk_axes.projected),
            (true, true, false),
            "SOUNDNESS — CR 608.2h, operative here and not decorative: this pump requires \
             \"information from the game (such as the number of creatures on the \
             battlefield)\", whose answer is \"determined only once, when the effect is \
             applied\". Pyreswipe Hawk's attack pump aggregates mana value over a LIVE \
             battlefield population, so EACH loop iteration re-determines it against a \
             larger board — which is precisely what makes the sequence's results \
             unpredictable under CR 732.2a. It must still read the sibling axis. A descent that relieved this too would be a blanket relief \
             wearing the descent's clothes. `event` is `true` for a reason this arm does \
             NOT own and must not be \"fixed\": an `Objects`-sourced `PropertyAggregate` walks its filter \
             under `FilterReadContext::LiveBoardCensus`, and `scan_target_filter`'s \
             `TargetFilter::Typed` arm sets `event: true` unconditionally (byte-preserved). \
             The blanket used to mask that; the descent exposes it unchanged. `projected` \
             is the axis the descent actually moves here (`true` under the blanket, `false` \
             now), so this triple discriminates the descent from the blanket on the \
             POSITIVE arm as well as on the negative one"
        );
    }

    /// A `Pump` whose magnitude reads a PROJECTED player resource keeps its veto, in a
    /// form the consuming firewall can see. `scan_quantity_ref` classifies
    /// `QuantityRef::LifeTotal` as `{event: false, sibling: false, projected: true}`,
    /// and that precision is a veto only because blocks (1b) and (2) of
    /// `analysis::resource`'s `fire_time_conditions_read_growing_class_scoped` consult
    /// [`ability_definition_reads_growing_class_for_loop`], whose `projected` half sees
    /// it (CR 608.2h: the answer is "determined only once, when the effect is
    /// applied"). The fixture is Loxodon Lifechanter's shipped `abilities[0]` body,
    /// verbatim, with `Ref(LifeTotal{Controller})` on BOTH load-bearing `PtValue`
    /// halves; axis isolation is asserted first and two paired controls keep it narrow.
    ///
    /// REVERT-PROBE: restore the `if acc.projected { Axes::CONSERVATIVE }` escalation ⇒
    /// the projected pump reports `(true, true, true)` ⇒ **FAILS**; narrow
    /// [`ability_definition_reads_growing_class_for_loop`] to `.sibling` ⇒ **FAILS**.
    #[test]
    fn projected_reading_pump_reports_its_axes_precisely_and_the_consumer_sees_them() {
        let projected_half = PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::Controller,
            },
        });

        // ── AXIS ISOLATION: the payload is projected-ONLY, which is the whole hazard ──
        let half = scan_pt_value(&projected_half, ScanMode::LoopFirewall);
        assert_eq!(
            (half.event, half.sibling, half.projected),
            (false, false, true),
            "AXIS ISOLATION: `QuantityRef::LifeTotal` is classified projected-ONLY. It is \
             the `sibling: false` half that makes this dangerous — a `.sibling`-only consult \
             could not see it. If this triple ever changes, the consult's `projected` half \
             stops being the thing under test"
        );

        let projected_pump = Effect::Pump {
            power: projected_half.clone(),
            toughness: projected_half,
            target: TargetFilter::SelfRef,
        };
        let axes = scan_effect(&projected_pump, ScanMode::LoopFirewall);
        assert_eq!(
            (axes.event, axes.sibling, axes.projected),
            (false, false, true),
            "CR 608.2h + CR 732.2a: a pump scaled by a life total requires \"information \
             from the game\", \"determined only once, when the effect is applied\", so each \
             loop iteration re-determines it against a different life total and the \
             sequence's results stop being predictable. The arm therefore reports the \
             payload's axes precisely, and `Axes::reads_growing_class` is what makes that \
             verdict a veto at blocks (1b) and (2). This payload is Loxodon Lifechanter's shipped \
             `abilities[0]` body verbatim: `Ref(LifeTotal{{Controller}})` on BOTH halves, \
             `SelfRef` target"
        );

        // ── THE CONSUMER: the precise verdict is what the firewall consult reads ─────
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        assert!(
            ability_definition_reads_growing_class_for_loop(&AbilityDefinition::new(
                AbilityKind::Activated,
                projected_pump.clone(),
            )),
            "the consult must see the `projected` half: `analysis::resource`'s blocks (1b) \
             and (2) ask this predicate and nothing else, so a `.sibling`-only reader would \
             relieve a life-total-scaled pump the pre-descent blanket vetoed"
        );

        // ── CONTROL 1: the relief this arm exists for is untouched ──────────────────
        let inert = scan_effect(
            &read_free_pump(TargetFilter::SelfRef),
            ScanMode::LoopFirewall,
        );
        assert!(
            !ability_definition_reads_growing_class_for_loop(&AbilityDefinition::new(
                AbilityKind::Activated,
                read_free_pump(TargetFilter::SelfRef),
            )),
            "paired negative at the consumer: the read-free pump reads neither axis, so the \
             consult relieves it and the widening cannot have become a blanket"
        );
        assert_eq!(
            (inert.event, inert.sibling, inert.projected),
            (false, false, false),
            "the arm relieves on the payload, so a read-free pump is still relieved. If \
             this flips, the narrowing became an all-or-nothing veto and P3's whole \
             deliverable (Chocobo Camp's Bird token) is gone with it"
        );

        // ── CONTROL 2: precision on the SIBLING axis survives the descent ──────────
        let hawk = scan_effect(&hawk_attack_pump(), ScanMode::LoopFirewall);
        assert_eq!(
            (hawk.event, hawk.sibling, hawk.projected),
            (true, true, false),
            "the aggregate pump's PRECISE triple must survive: this arm classifies the \
             payload, never \"reads anything\". `scan_target_filter`'s \
             `TargetFilter::Typed` arm sets `event: true` unconditionally, so an \
             all-or-nothing guard would swallow this into the blanket AND veto every \
             `Typed`-targeted pump — DERIVED from that unconditional `true`, never run: it \
             reddens `pump_with_board_reading_target_still_vetoes`'s arm B"
        );
    }

    /// AXIS ISOLATION for the projected-only SHAPES `analysis::resource`'s
    /// projected-surface rows rest on. Those rows read the `sibling` ∨ `projected`
    /// disjunction and cannot say which axis carried it, and the exported SINGLE-axis
    /// projections over these leaf types all scan in `ScanMode::Conservative` while the
    /// claim here is about the `LoopFirewall` verdict — so the property is
    /// pinned here, where [`Axes`] is reachable. It belongs to the SHAPE, not to any
    /// one fixture. (`QuantityRef::LifeTotal` is pinned by
    /// [`projected_reading_pump_reports_its_axes_precisely_and_the_consumer_sees_them`].)
    ///
    /// REVERT-PROBE: give any one of these arms `sibling: true` ⇒ **FAILS** on that
    /// arm's label.
    #[test]
    fn projected_only_leaves_carry_no_sibling_axis() {
        use crate::types::ability::TypeFilter;

        let target = TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::ControllerMatches {
                player: Box::new(PlayerFilter::OpponentLostLife),
            }],
        };
        // Under `LoopFirewall` the `TargetFilter::Typed` arm takes its `sibling` verbatim
        // from `typed_filter_axes`, so this IS that arm's verdict for this shape.
        let by_target = typed_filter_axes(&target, ScanMode::LoopFirewall);
        let by_condition = scan_ability_condition(
            &AbilityCondition::NthResolutionThisTurn { n: 2 },
            ScanMode::LoopFirewall,
        );
        let refs = [
            (
                "LifeGainedThisTurn",
                QuantityRef::LifeGainedThisTurn {
                    player: PlayerScope::Controller,
                },
            ),
            (
                "LifeLostThisTurn",
                QuantityRef::LifeLostThisTurn {
                    player: PlayerScope::Controller,
                },
            ),
            (
                "SpellsCastThisTurn",
                QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
            ),
            (
                "ZoneChangeCountThisTurn",
                QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Graveyard),
                    filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                },
            ),
        ];
        for (label, axes) in [
            ("ControllerMatches{OpponentLostLife}", by_target),
            ("NthResolutionThisTurn", by_condition),
        ]
        .into_iter()
        .chain(
            refs.iter()
                .map(|(l, q)| (*l, scan_quantity_ref(q, ScanMode::LoopFirewall))),
        ) {
            // The `event` axis is not part of the claim: `scan_target_filter`'s `Typed`
            // arm sets it unconditionally, so a filter-bearing shape carries it either
            // way. The growing class is `sibling` ∨ `projected` (CR 732.2a) and that
            // pair is what these rows rest on.
            assert_eq!(
                (axes.sibling, axes.projected),
                (false, true),
                "AXIS ISOLATION ({label}): CR 732.2a — this shape is classified \
                 projected-ONLY, and the `sibling: false` half is exactly what makes it a \
                 hazard a `.sibling`-only consult could not see. If it ever gains \
                 `sibling`, `analysis::resource`'s projected-surface rows stop testing the \
                 axis they name"
            );
        }
    }

    /// `ScanMode::Conservative` is byte-identical for BOTH shapes.
    ///
    /// This is the row that keeps the CR 603.3b trigger-ordering gate and every
    /// non-firewall consumer unmoved, and it is why `LoopDetectionMode::Off` games stay
    /// byte-identical.
    ///
    /// REVERT-PROBE: delete the `ScanMode::Conservative => Axes::CONSERVATIVE` arm (let the
    /// descent run in both modes) ⇒ the read-free case reports `(false, false, false)` ⇒
    /// **FAILS**.
    #[test]
    fn scan_effect_pump_stays_conservative_in_conservative_mode() {
        for (label, effect) in [
            ("read-free", read_free_pump(TargetFilter::SelfRef)),
            ("aggregate-bearing", hawk_attack_pump()),
        ] {
            let axes = scan_effect(&effect, ScanMode::Conservative);
            assert_eq!(
                (axes.event, axes.sibling, axes.projected),
                (true, true, true),
                "{label}: CR 603.3b — under `Conservative` the `Effect::Pump` arm must stay \
                 the byte-identical fail-closed blanket every non-firewall consumer already \
                 sees. Only the CR 732.2a firewall may observe the descent"
            );
        }
    }

    /// A `Pump` whose TARGET reads the board still vetoes (CR 732.2a: a target naming
    /// a live board population is itself a sibling read). The three-way family
    /// `analysis::resource::pump_target_axis_is_not_blind` already uses, at the scanner
    /// level: three defs differing ONLY in `target`. The read-free `PtValue::Fixed`
    /// halves are not a convenience — they make `target` the SOLE possible source of a
    /// sibling read, which a Pyreswipe Hawk fixture could not do (its `power` is a
    /// `PropertyAggregate` over `Objects`, and `scan_quantity_ref` sets `sibling: true` for that
    /// variant BEFORE walking the filter). ALL THREE verdicts are asserted, so a
    /// descent that dropped the target leg reddens arm C and one that vetoed on any
    /// target reddens arms A and B.
    ///
    /// REVERT-PROBE: delete `acc = acc.or(scan_target_filter(target, target_ctx, mode));`
    /// from the `LoopFirewall` half of the `Effect::Pump` arm ⇒ arm C reports `false` ⇒
    /// **FAILS**.
    #[test]
    fn pump_with_board_reading_target_still_vetoes() {
        let bare_typed = TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            ..Default::default()
        });
        let board_reading = TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            properties: vec![FilterProp::DifferentNameFrom {
                filter: Box::new(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![],
                })),
            }],
            ..Default::default()
        });
        for (label, target, expected) in [
            ("A `SelfRef`", TargetFilter::SelfRef, false),
            ("B bare `Typed{Creature}`", bare_typed, false),
            (
                "C board-reading `Typed{Creature, DifferentNameFrom}`",
                board_reading,
                true,
            ),
        ] {
            let axes = scan_effect(&read_free_pump(target), ScanMode::LoopFirewall);
            assert_eq!(
                axes.sibling, expected,
                "{label}: the P/T halves are `PtValue::Fixed` on all three, so this verdict \
                 is the TARGET leg's and nothing else. CR 732.2a: a target naming a live \
                 board population reads the growing class; a self-reference and a bare \
                 type predicate do not"
            );
        }
    }

    /// Chocobo Camp's `abilities[1]` stops reading the sibling axis.
    ///
    /// The real card, from its VERBATIM Oracle text (MTGJSON `AtomicCards.json`). Its
    /// `Effect::Pump` sits four path segments below `effect` —
    /// `abilities[1].effect.static_abilities[0].modifications[0].trigger.execute.effect`
    /// — reached by `scan_effect`'s `Effect::Token` descent →
    /// `scan_continuous_modification` → `scan_trigger_definition` →
    /// `ability_definition_axes` → `scan_effect`, so the arm applies at whatever depth
    /// the recursion reaches it. REACH GUARD, on the SAME descent path: `abilities[1]`
    /// with the granted trigger's pump `power` swapped for an `ObjectCount`-scaled
    /// quantity must STILL read the growing class, so a global kill switch fails here.
    ///
    /// REVERT-PROBE: restore `Effect::Pump { .. } => Axes::CONSERVATIVE` ⇒ `abilities[1]`
    /// reads the sibling axis again ⇒ **FAILS**.
    #[test]
    fn chocobo_camp_idx1_no_longer_reads_the_sibling_axis() {
        let parsed = crate::parser::parse_oracle_text(
            "This land enters tapped unless you control a legendary creature.\n\
             {T}: Add {G}. When you next cast a Bird creature spell this turn, it enters with \
             an additional +1/+1 counter on it.\n\
             {2}{G}{G}, {T}: Create a 2/2 green Bird creature token with \"Whenever a land you \
             control enters, this token gets +1/+0 until end of turn.\"",
            "Chocobo Camp",
            &[],
            &["Land".to_string()],
            &[],
        );
        assert_eq!(
            parsed.abilities.len(),
            2,
            "fixture pin: Chocobo Camp parses to exactly TWO activated abilities; a parser \
             change that splits or merges them re-points this row"
        );

        // VACUITY GUARD: the token body must really carry the granted trigger whose execute
        // is the `Effect::Pump` under test. Without this, a parse change that dropped the
        // static ability entirely would turn `abilities[1]` read-free for the WRONG reason
        // and this row would go green while proving nothing about the descent.
        let Effect::Token {
            static_abilities, ..
        } = parsed.abilities[1].effect.as_ref()
        else {
            panic!("fixture pin: `abilities[1]` must be the token-creating activated ability");
        };
        let granted = static_abilities
            .iter()
            .flat_map(|sd| sd.modifications.iter())
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            })
            .expect(
                "fixture pin: the token grants a trigger (\"Whenever a land you control enters\")",
            );
        assert!(
            matches!(
                granted
                    .execute
                    .as_deref()
                    .expect("fixture pin: the granted trigger carries an execute body")
                    .effect
                    .as_ref(),
                Effect::Pump { .. }
            ),
            "fixture pin: the granted trigger's execute body is the `Effect::Pump` this row \
             is about — the descent has to reach it four segments below `effect`"
        );

        // REACH GUARD, on the same descent path: give the granted trigger's pump an
        // `ObjectCount`-scaled `power` and the identical walk must still veto.
        let mut reading = parsed.abilities[1].clone();
        let Effect::Token {
            static_abilities, ..
        } = reading.effect.as_mut()
        else {
            panic!("fixture pin: `abilities[1]` must be the token-creating activated ability");
        };
        let reading_trigger = static_abilities
            .iter_mut()
            .flat_map(|sd| sd.modifications.iter_mut())
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            })
            .expect("fixture pin: the token grants a trigger");
        let reading_exec = reading_trigger
            .execute
            .as_deref_mut()
            .expect("fixture pin: the granted trigger carries an execute body");
        let Effect::Pump { power, .. } = reading_exec.effect.as_mut() else {
            panic!("fixture pin: the granted trigger's execute body is an `Effect::Pump`");
        };
        *power = PtValue::Quantity(object_count());
        assert!(
            ability_definition_axes(&reading, ScanMode::LoopFirewall).sibling,
            "reach guard: with the granted trigger's pump scaled by a board `ObjectCount`, \
             the SAME four-segment descent must still read the sibling axis. If this flips, \
             the relief below is a global kill switch rather than a payload descent and \
             proves nothing"
        );

        // THE CLAIM.
        assert!(
            !ability_definition_axes(&parsed.abilities[1], ScanMode::LoopFirewall).sibling,
            "CR 732.2a: the granted trigger's body is a `Pump{{Fixed(1), Fixed(0), \
             SelfRef}}`. That it requires no information from the game is an AST property \
             (both `PtValue`s are literals), not a rules one, so no CR is cited for it. \
             CR 732.2a is what makes it decisive: the proposed sequence's results stay \
             predictable and no action in it is conditional, so the object-growth firewall \
             must not veto the shortcut offer on it"
        );
    }

    /// A fixed anthem modification reads nothing (the control for the dynamic case).
    #[test]
    fn fixed_anthem_modification_reads_nothing() {
        let axes = scan_continuous_modification(
            &ContinuousModification::AddPower { value: 2 },
            ScanMode::LoopFirewall,
        );
        assert!(!axes.event && !axes.sibling && !axes.projected);
    }

    /// A dynamic-P/T modification reads a sibling aggregate.
    /// Revert-probe: move the arm into the read-free bucket (⇒ NONE) ⇒ fails.
    #[test]
    fn dynamic_pt_modification_reads_sibling() {
        let m = ContinuousModification::SetDynamicPower {
            value: object_count(),
        };
        assert!(scan_continuous_modification(&m, ScanMode::LoopFirewall).sibling);
    }

    /// A token whose `enter_with_counters` count is a board `ObjectCount`
    /// reads sibling. The vanilla control has empty counters ⇒ NONE, so the
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

    /// A token whose granted static ability carries a dynamic-P/T modification
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

    /// A token carrying a growing-cost keyword (Convoke — a UNIT variant
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

    /// `AddCounterOnEnter` with a dynamic count reads sibling — it looks
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

    /// A board-color aggregate self-asserts its OWN `sibling`, even
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

    /// `TriggerEventManaType` reads the triggering event (event axis).
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

    /// Gaea's Cradle's `{T}: Add {G} for each creature you control` parses to
    /// `AnyOneColor{count: Ref(ObjectCount{Typed{Creature}}), ...}` — a COUNT-path arm
    /// whose sibling comes from `count` → `scan_quantity_ref::ObjectCount`, NOT from a
    /// board-aggregate literal (the distinct FILTER path is the board-color row above).
    /// Revert-probe: bind the mana arm's `count` to `_` ⇒ flips to NONE.
    #[test]
    fn gaeas_cradle_count_path_vetoes() {
        let p = ManaProduction::AnyOneColor {
            count: object_count(),
            color_options: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        };
        assert!(scan_mana_production(&p, ScanMode::LoopFirewall).sibling);
    }

    // ---- CR 732.2a Typed-precision census discipline ----

    /// The census-discipline structural invariant. Under `LoopFirewall`,
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

    /// Each `LiveBoardCensus` HOLE arm (including `ControllerControlsMatching`
    /// and `ZoneCardCount`) yields `sibling:true`
    /// under LoopFirewall with a bare `Typed{Creature}` — the census base, NOT the
    /// (relaxed) `Typed` arm, carries the veto. The control proves it is load-bearing:
    /// the SAME bare `Typed` under `SnapshotOrEvent` relaxes to `sibling:false`, so
    /// flipping any arm's ctx to `SnapshotOrEvent` would relax it into a false
    /// certificate.
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
        // ControllerControlsMatching (a live board census).
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
        // ZoneCardCount (a battlefield-scoped census, unconditional fail-closed).
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

    /// CR 614.1d + CR 732.2a: the `UnlessControlsSubtype` arm reports the census its
    /// evaluator runs — the live-board `sibling` axis and nothing else, so the verdict is the
    /// narrow literal and not `Axes::CONSERVATIVE`. Asserted on the raw axes in both scan
    /// modes and through the two production accessors the firewall consults; the `event`
    /// conjunct goes direct because neither accessor exposes that axis.
    ///
    /// REVERT / MUTATION PROBE: restore `=> Axes::NONE` ⇒ the `sibling` assertions FAIL;
    /// replace the arm with `=> Axes::CONSERVATIVE` ⇒ the `projected` and `event` assertions
    /// FAIL. Both directions redden this one row.
    #[test]
    fn unless_controls_subtype_reports_the_census_it_runs() {
        use crate::types::ability::{ReplacementCondition, TurnUpCostSource};
        let subtype = |subs: &[&str]| ReplacementCondition::UnlessControlsSubtype {
            subtypes: subs.iter().map(|s| (*s).to_string()).collect(),
        };
        let dragonskull = subtype(&["Swamp", "Mountain"]);

        for mode in [ScanMode::Conservative, ScanMode::LoopFirewall] {
            let axes = scan_replacement_condition(&dragonskull, mode);
            assert!(
                axes.sibling,
                "the evaluator walks the battlefield for another controlled permanent carrying \
                 a listed subtype, so the scan must report the sibling axis in every mode"
            );
            assert!(
                !axes.projected,
                "the evaluator reads no player-level monotone resource, so the narrow form must \
                 not widen into Axes::CONSERVATIVE"
            );
            assert!(
                !axes.event,
                "the evaluator reads no triggering-event characteristic, so the narrow form \
                 must not widen into Axes::CONSERVATIVE"
            );
        }

        assert!(
            replacement_condition_reads_sibling_mutable(&dragonskull),
            "the accessor block (3) actually consults must carry the census verdict, not only \
             the private walk"
        );
        assert!(
            !replacement_condition_reads_projected_resource(&dragonskull),
            "the projected accessor must stay false — a board census is not a player-resource \
             read"
        );

        // Each axis owes a fixture that moves it, and the sibling axis one that leaves it
        // false; without them these conjuncts are satisfied by a scanner that answers the
        // same way for every condition.
        assert!(
            replacement_condition_reads_sibling_mutable(
                &ReplacementCondition::UnlessControlsMatching {
                    filter: TargetFilter::Typed(TypedFilter::creature())
                }
            ),
            "positive control: the cluster sibling that delegates its census reports the \
             sibling axis"
        );
        assert!(
            !replacement_condition_reads_sibling_mutable(&ReplacementCondition::UnlessYourTurn),
            "negative control: a turn-order condition censuses nothing, so this accessor can \
             still answer false"
        );
        assert!(
            replacement_condition_reads_projected_resource(
                &ReplacementCondition::UnlessPlayerLifeAtMost { amount: 5 }
            ),
            "positive control: the projected accessor can answer true, so the false above is a \
             verdict and not a dead axis"
        );
        assert!(
            scan_replacement_condition(
                &ReplacementCondition::TurnUpCostSourcePaid {
                    source: TurnUpCostSource::Megamorph
                },
                ScanMode::Conservative
            )
            .event,
            "positive control: the event axis can answer true, so the false above is a verdict \
             and not a dead axis"
        );

        assert!(
            replacement_condition_reads_sibling_mutable(&subtype(&[])),
            "an empty subtype list makes the census ANSWER vacuously false while the walk still \
             happens, so relaxing the axis by inspecting the payload is wrong"
        );
        let leq = scan_replacement_condition(
            &ReplacementCondition::UnlessControlsOtherLeq {
                count: 2,
                filter: TypedFilter::land(),
            },
            ScanMode::Conservative,
        );
        assert!(
            leq.sibling && leq.projected && leq.event,
            "untouched cluster sibling: a red here says the edit reached this arm too, i.e. the \
             whole `UnlessControls*` cluster was retuned rather than the one arm this row pins"
        );
        assert!(
            replacement_condition_reads_sibling_mutable(&ReplacementCondition::And {
                conditions: vec![dragonskull, ReplacementCondition::UnlessYourTurn],
            }),
            "the And recursion ors the axes, so a compound inherits the subtype arm's census \
             verdict; paired with the UnlessYourTurn negative control above, the true here is \
             attributable to that arm"
        );
    }

    /// Source text of a top-level `fn`, from its signature to the column-0 `}` that
    /// closes it.
    ///
    /// The end anchor is STRUCTURAL — rustfmt puts a top-level closing brace at column
    /// 0 and nothing inside a body there — so no comment edit can move or delete it.
    /// The rows below previously ended their slice at a prose `// ----` divider; a
    /// comment prune deleted it and all three panicked. An anchor a comment audit can
    /// erase is not an anchor.
    ///
    /// Narrowing to the body is also the safe direction for what these rows measure:
    /// the slice cannot reach `Effect::` names belonging to a LATER function, so a
    /// derived tag set can only lose members (loud: the `want` lists mismatch), never
    /// silently gain them.
    fn top_level_fn_src(header: &str) -> &'static str {
        let src = include_str!("ability_scan.rs");
        let start = src
            .find(header)
            .unwrap_or_else(|| panic!("{header}: no such top-level fn"));
        let len = src[start..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{header}: no column-0 closing brace"));
        &src[start..start + len]
    }

    /// The `LiveBoardCensus` tag set of `effect_target_ctx` == EXACTLY the
    /// enumeration-derived MASS-POPULATION set, source-scanned rather than
    /// hand-counted. Under
    /// B's SnapshotOrEvent default this is the primary false-certificate gate: only a
    /// census tag vetoes a mass read that ESCALATES over inert token growth (which
    /// `grown_objects_are_inert` cannot catch — obligation-(ii) is never a relax
    /// license for a census read). A dropped tag (a mass slot silently on the Snapshot
    /// default) OR an added one turns this RED, forcing a conscious re-audit.
    #[test]
    fn census_tag_set_is_exactly_enumerated() {
        let fnsrc = top_level_fn_src("fn effect_target_ctx(");
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
            "ChooseCounterKind",
            "ChooseObjectsIntoTrackedSet",
            "ChoosePermanent",
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
            // Suspect/Unsuspect scope:All are mass-population battlefield reads
            // (`suspect.rs` enumerates `state.battlefield`, `target_filter()`==None).
            // Their `Effect::` name appears in the census `|`-chain scope-gated on
            // `EffectScope::All`; the scope:Single arms live in the relax group below and
            // are NOT scanned here (they sit past the census terminator).
            "Suspect",
            // CR 701.27a + CR 115.10a: mass Transform (scope:All) enumerates
            // `state.battlefield` (`transform_effect::resolve_all`). Scope-gated on
            // `EffectScope::All` in the census `|`-chain; the scope:Single arm sits past
            // the census terminator in the relax group and is NOT scanned here.
            "Transform",
            "UnattachAll",
            "Unsuspect",
            // Dual-mode mass-battlefield resolvers (a resolver
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
        assert_eq!(got.len(), 31, "exactly 31 mass-population census tags");
    }

    /// With `SnapshotOrEvent` the DEFAULT, the obligation-(ii)-PROVEN census-role
    /// exception set == EXACTLY {SetTapState}. SetTapState is census-ROLE ("tap/untap
    /// all", scope All) yet relaxes because tap-state is state-convergent/idempotent
    /// (a specific proven non-escalation, NOT a general (b)-license). Structurally it is
    /// the SOLE effect with a DEDICATED SnapshotOrEvent arm (the region between the
    /// census arm and the single-object group); giving any OTHER census-role slot a
    /// dedicated Snapshot arm turns this RED. Dual-guard with
    /// `census_tag_set_is_exactly_enumerated`, which pins the census tag set.
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
        let fnsrc = top_level_fn_src("fn effect_target_ctx(");
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

    /// CR 701.60a: Suspect/Unsuspect census classification is SCOPE-SENSITIVE,
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

    /// CR 732.2a: the independent census PARTITION (`effect_census_role`) agrees
    /// with `effect_target_ctx` on the Census/Relax boundary, closing the gap where a
    /// census-ROLE slot silently in the generic relax `|`-chain (exactly Suspect{All})
    /// is invisible to the census-arm-only guards. Structural: both functions' `Census`
    /// name-sets are source-scanned and asserted IDENTICAL. Behavioral: the
    /// two oracles agree on every discriminator, incl. BOTH Suspect/Unsuspect scopes.
    ///
    /// REVERT-PROBE (discrimination proof): moving `Suspect{All}` out of the census arm of
    /// EITHER function breaks this guard — if only `effect_target_ctx` is reverted the
    /// source-scanned census sets diverge (structural `assert_eq!` fails); if
    /// `effect_census_role` misclassifies it as `Relax` the behavioral `Census` assertion
    /// flips.
    #[test]
    fn census_partition_agrees_with_effect_target_ctx() {
        use crate::types::ability::{EffectScope, TapStateChange};
        use ScanMode::LoopFirewall;

        // -- Structural: the two census name-sets are byte-identical.
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
        let etc = top_level_fn_src("fn effect_target_ctx(");
        let ecr = top_level_fn_src("fn effect_census_role(");
        let etc_census = census_names(etc, "=> FilterReadContext::LiveBoardCensus,");
        let ecr_census = census_names(ecr, "=> CensusRole::Census,");
        assert_eq!(
            etc_census, ecr_census,
            "effect_census_role Census set diverged from effect_target_ctx"
        );
        assert_eq!(ecr_census.len(), 31, "exactly 31 census members");

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
        census(
            &Effect::ChooseCounterKind {
                target: f(),
                domain: Default::default(),
                chooser: Default::default(),
            },
            true,
        );
        // CR 701.27a + CR 115.10a: mass Transform is a battlefield census in BOTH oracles
        // (scope:All), and a bounded single-target read (scope:Single) that relaxes. It is
        // a true Census, NOT the SetTapState relax exception (ObjectPt/ability write).
        census(
            &Effect::Transform {
                target: f(),
                scope: EffectScope::All,
            },
            true,
        );
        census(
            &Effect::Transform {
                target: f(),
                scope: EffectScope::Single,
            },
            false,
        );
        assert_eq!(
            effect_census_role(&Effect::Transform {
                target: f(),
                scope: EffectScope::All,
            }),
            CensusRole::Census,
            "mass Transform must be a true Census, not the SetTapState relax exception"
        );

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

    /// CR 732.2a: the dual-mode mass-battlefield resolvers each census in BOTH
    /// oracles under `LoopFirewall`. Each
    /// enumerates the battlefield and applies the effect to EVERY matching object (scales
    /// with the growing class) — six via a dual-mode "no explicit target ⇒ mass scan"
    /// fallback, `CopyTokenBlockingAttacker` UNCONDITIONALLY — so relaxing its filter read
    /// risks a false combo certificate. There is no static discriminator between
    /// announced-single and mass modes, so the entire variant censuses.
    ///
    /// DISCRIMINATING (revert-probe): moving ANY one of these back into the
    /// `SnapshotOrEvent` relax arm of `effect_target_ctx` (or the `Relax(_)` arm of
    /// `effect_census_role`) flips its assertion below to a mismatch, turning this RED.
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

    /// CR 732.2a: DURABLE forward-protection for the silent-miss class
    /// at the RESOLVER layer. Scans every `game/effects/*.rs` source for the (broadened)
    /// MASS-BATTLEFIELD-SCAN idiom and asserts the matching file set == a curated
    /// classification. A NEW resolver file that adds the idiom (or a curated file that
    /// stops matching) fails this test until a human re-classifies — the resolver-level
    /// analogue of the census oracles' no-wildcard forcing. HEURISTIC defense-in-depth,
    /// NOT a proof: the exhaustive `effect_census_role` oracle is the completeness
    /// authority.
    ///
    /// Idiom = union of two signals, keyed for RECALL over precision (a
    /// `from_targets`-gated key misses the guard-varied mass reads, so the detector
    /// does no guard gating at all):
    ///   (a) a reference to `resolved_battlefield_object_ids(` — the shared dual-mode
    ///       helper (`turn_face_up`/`turn_face_down` delegate with zero inline scan,
    ///       so this call-substring is load-bearing, not optional);
    ///   (b) a battlefield-population enumeration (`battlefield_phased_in_ids` /
    ///       `zone_object_ids`) filtered by `matches_target_filter`, whatever the guard.
    /// A false positive costs one Relax entry; a missed mass read is a soundness gap,
    /// so the flood is CLASSIFIED, never re-narrowed with an allowlist.
    ///
    /// KNOWN BLIND SPOT: signal (b) keys on the helper enumerators, so a resolver
    /// iterating via raw `state.battlefield.iter()` / `.values()` is NOT flagged.
    /// Raw-iter MASS reads (`GainActivatedAbilitiesOfTarget`, `BecomeCopy`) are still
    /// census in the `effect_census_role` oracle, so soundness holds. Bounded raw-iter
    /// / O(1) reads are deliberately kept OUT of `CLASSIFIED` so the set-equality stays
    /// over the idiom-matched files: `vote.rs` (`votes_per_session_for` is snapshotted
    /// at session start) and `switch_pt.rs` (O(1) `state.battlefield.contains()` over
    /// the effect's own `ids`). Their variants correctly RELAX in the oracle.
    ///
    /// NON-VACUITY: (i) deleting/adding any file in `CLASSIFIED` makes
    /// `matched != curated`; (ii) reverting a census-tag drops the variant from the
    /// source-scanned `effect_census_role` census set, so the census-tie fails.
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
                "pump.rs",
                true,
                "PumpAll (pump_all_affected_objects): battlefield_phased_in_ids mass pump, \
                 a read that scales with the board; single Pump path also present in-file. \
                 Joined the idiom in #7484, when the producer moved off a raw \
                 state.battlefield scan onto the same enumeration goad.rs uses",
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
        let ecr = top_level_fn_src("fn effect_census_role(");
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
            ("pump.rs", "PumpAll"),
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

    /// Byte-identity: the self-asserting `QuantityRef` board-census arms yield
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

    // ---- determinism gate: the randomness classifier (CR 732.2a) ----
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
    fn noted_mana_effect_is_read_free_and_deterministic() {
        let effect = Effect::NoteManaSpent;
        let axes = scan_effect(&effect, ScanMode::LoopFirewall);
        assert!(!axes.event && !axes.sibling && !axes.projected);
        assert_eq!(
            effect_target_ctx(&effect, ScanMode::LoopFirewall),
            FilterReadContext::SnapshotOrEvent
        );
        assert_eq!(
            effect_census_role(&effect),
            CensusRole::Relax(RelaxReason::BoundedOrNoPopulation)
        );
        assert!(!effect_is_randomness_bearing(&effect));
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
        // Player-counter axis (CR 122.1) — experience has NO
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

    // ---- Axis 3: object/board readers are NON-reading ----
    #[test]
    fn object_and_board_readers_are_not_projected() {
        // Object counter / P/T reads are strict-compared by gate (1), not projected.
        for qty in [
            QuantityRef::Power {
                scope: ObjectScope::Source,
            },
            QuantityRef::BasePower {
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
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::BasePower {
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

    /// CR 400.7d + CR 601.2h: the Adamant ability rider ("if at least three red mana
    /// was spent to cast this spell, it deals 4 damage instead") parses to the generic
    /// `QuantityCheck { ManaSpentToCast { .., OfColor } }` shape rather than the legacy
    /// `AbilityCondition::ManaColorSpent`, so it scans on the `Axes::CONSERVATIVE` arm
    /// (via `QuantityCheck` → `scan_quantity_expr`) instead of `ManaColorSpent`'s
    /// `Axes::NONE`. All three axes read true, which is the intended direction:
    /// `event` is CORRECT rather than merely conservative, because CR 400.7d makes the
    /// paid-mana record a cost-paid-object characteristic; `sibling` / `projected` are
    /// over-inclusive but fail-SAFE, since the record is stamped once at CR 601.2h and
    /// a `true` can only make the analysis prompt, never auto-resolve.
    ///
    /// Neither `ordering_parity_sweep` (which imports `ability_rw` only) nor
    /// `coverage-data.json` observes this axis, hence this direct assertion.
    #[test]
    fn adamant_rider_generic_shape_reads_all_scan_axes() {
        let generic = AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::OfColor {
                        color: ManaColor::Red,
                    },
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        };
        // SCOPE OF THIS TEST: it pins the scan-axis CLASSIFICATION of both
        // shapes. It constructs the conditions directly and never invokes the
        // parser, so it does NOT detect a revert of the `OfColor` lowering —
        // that is guarded by `leading_word_mana_spent_condition_parses_adamant`
        // in `parser/oracle_effect/conditions.rs`. The legacy pin below is what
        // keeps the delta explicit.
        let generic_axes = scan_ability_condition(&generic, ScanMode::Conservative);
        assert!(generic_axes.event);
        assert!(generic_axes.sibling);
        assert!(generic_axes.projected);
        // The public projection consumed by `analysis::resource` agrees.
        assert!(ability_condition_reads_projected_resource(&generic));

        // Pin the legacy shape's classification so the delta is explicit and a
        // future retirement of `ManaColorSpent` cannot silently change it.
        let legacy = AbilityCondition::ManaColorSpent {
            color: ManaColor::Red,
            minimum: 3,
        };
        let legacy_axes = scan_ability_condition(&legacy, ScanMode::Conservative);
        assert!(!legacy_axes.event);
        assert!(!legacy_axes.sibling);
        assert!(!legacy_axes.projected);
        assert!(!ability_condition_reads_projected_resource(&legacy));
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

    // ---- the CR 608.2i ledger read is an axis-2 board read ----

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

    /// `battlefield_entries_this_turn` is APPENDED to by every battlefield entry
    /// (`record_battlefield_entry`), so a read of it is a board-derived AGGREGATE and
    /// must self-assert `sibling: true` — CR 732.2a: the object-growth firewall may
    /// only ever OVER-veto, never certify a bounded loop a live observer disproves.
    ///
    /// VACUITY TRAP: assertion (1) is `ScanMode::Conservative`, where the
    /// `TargetFilter::Typed` arm already forces `sibling: true`, so it is NOT the
    /// discriminator. Assertion (2) is `ScanMode::LoopFirewall` — the mode
    /// `analysis::resource::fire_time_conditions_read_growing_class` uses — and asks
    /// the `sibling` axis ALONE. `Axes::reads_growing_class` cannot discriminate here:
    /// this arm's `projected: true` keeps the disjunction green either way.
    ///
    /// REVERT-PROBE: zero this arm's `sibling` alone, leaving `projected: true` →
    /// (2) and (3) FAIL while (1) and (4) still pass.
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
            "(1) Conservative axis-2 — passes with or without Step 0c",
        );
        // (2) THE DISCRIMINATOR — the SIBLING half of the growing class, under
        // LoopFirewall, the CR 732.2a firewall's mode.
        assert!(
            ability_definition_axes(&ledger, ScanMode::LoopFirewall).sibling,
            "(2) CR 732.2a: the ledger read must carry the SIBLING axis under \
             LoopFirewall, the mode the two `..._for_loop` scan sites in \
             `analysis::resource` use. Those sites read `sibling ∨ projected`, and \
             this arm's `projected` predates Step 0c, so only the sibling half \
             distinguishes a board-aggregate read from a plain per-turn journal",
        );
        // (3) parity guard — the look-back and live siblings must agree on the
        // sibling axis in BOTH scan modes.
        assert_eq!(
            (
                ability_definition_reads_sibling_mutable(&ledger),
                ability_definition_axes(&ledger, ScanMode::LoopFirewall).sibling,
            ),
            (
                ability_definition_reads_sibling_mutable(&live),
                ability_definition_axes(&live, ScanMode::LoopFirewall).sibling,
            ),
            "(3) parity: `BattlefieldEntriesThisTurn` and `EnteredThisTurn` are the \
             same board-aggregate class on the sibling axis, in both scan modes",
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

    /// The non-vacuity instrument for the row above. Proves the SHIPPED
    /// Park Heights Pegasus face is what
    /// `analysis::resource::fire_time_conditions_read_growing_class`
    /// visits, and that the flip lands on the trigger `execute` scan rather than
    /// on the Conservative trigger-`condition` path that already forces
    /// `sibling: true` (the Gargoyle Flock trap: `true → true`, non-discriminating).
    ///
    /// REVERT-PROBE: zero the ledger arm's `sibling` alone, leaving `projected: true`
    /// → (2) FAILS.
    #[test]
    fn bbfu10_shipped_ledger_observer_flips_for_loop_axis() {
        let db = crate::test_support::shared_card_db();
        let face = db
            .face_index
            .get("park heights pegasus")
            .expect("Park Heights Pegasus is in tests/fixtures/integration_cards.json.gz");

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

        // (2) THE DISCRIMINATOR — the SIBLING half of what the trigger-body scan site
        // calls. That site reads `sibling ∨ projected`, which is green on the
        // `projected` half alone and so cannot be this row's instrument.
        assert!(
            ability_definition_axes(execute, ScanMode::LoopFirewall).sibling,
            "(2) CR 732.2a: a shipped ledger observer must veto an object-growth \
             certificate on the SIBLING axis — its `projected` half predates Step 0c \
             and cannot tell this body from one that never reads the board",
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
            !ability_definition_reads_growing_class_for_loop(plain_execute),
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
        assert!(ability_reads_sibling_mutable(&ability_with_amount(
            QuantityRef::BasePower {
                scope: ObjectScope::Source
            }
        )));
        // Fixed drain reads no sibling-mutable state — safe to auto-resolve.
        assert!(!ability_reads_sibling_mutable(&fixed_drain()));
    }

    // ---- scry look count is event-context ----

    /// CR 701.22a + CR 603.2c: "the number of cards looked at while scrying
    /// this way" (Elrond, Master of Healing) reads the CURRENT trigger's
    /// preserved scry event — axis 1 (event-context), mirroring
    /// `QuantityRef::EventContextAmount` — not an inert transient scalar.
    #[test]
    fn triggering_scry_look_count_reads_event_context() {
        assert!(ability_uses_event_context(&ability_with_amount(
            QuantityRef::TriggeringScryLookCount
        )));
    }

    /// CR 701.22a + CR 701.22d + CR 603.2c: the completed-scry bottom count is
    /// carried by the current trigger event, never a sibling or projected
    /// resource. Assert the scanner axes directly so the shared match arm cannot
    /// accidentally classify it more broadly.
    #[test]
    fn triggering_scry_bottom_count_has_only_the_event_axis() {
        let axes = scan_quantity_ref(
            &QuantityRef::TriggeringScryBottomCount,
            ScanMode::Conservative,
        );
        assert!(axes.event);
        assert!(!axes.sibling);
        assert!(!axes.projected);
    }

    /// `TargetFilter::PlayerMatching` recursion is CLASSIFIED, not
    /// blind-defaulted to `Axes::NONE`.
    ///
    /// CR 102.1: the nested `PlayerFilter` can read projected per-player state
    /// (`PlayerAttribute` over a life total) and can box a whole `TargetFilter`
    /// (`ControlsCount`). Reporting `Axes::NONE` for it would let trigger
    /// ordering auto-resolve a group whose members really do read
    /// order-relevant state.
    ///
    /// Revert-failing: replace the recursive arm with `Axes::NONE` and the
    /// `projected` assertion below flips.
    #[test]
    fn player_matching_scan_recurses_into_the_nested_player_filter() {
        let payload = PlayerFilter::PlayerAttribute {
            relation: crate::types::ability::PlayerRelation::All,
            attr: Box::new(QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            }),
            comparator: Comparator::GT,
            value: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
            }),
        };
        let scanned = scan_target_filter(
            &TargetFilter::PlayerMatching {
                player: Box::new(payload.clone()),
            },
            FilterReadContext::SnapshotOrEvent,
            ScanMode::Conservative,
        );
        let direct = scan_player_filter(&payload, ScanMode::Conservative);

        // The carrier must report exactly what its payload reports, on every axis.
        assert_eq!(scanned.event, direct.event);
        assert_eq!(scanned.sibling, direct.sibling);
        assert_eq!(scanned.projected, direct.projected);
        // …and that report must be non-empty: a life-total predicate reads
        // projected per-player state, so `Axes::NONE` would be a blind default.
        assert!(
            scanned.event || scanned.sibling || scanned.projected,
            "PlayerMatching over a life-total predicate must not scan as NONE"
        );
    }

    /// Review #7820 round 5: the condition consumes the triggering event's
    /// snapshotted bearer — event-bound, so two distinct temptations are never
    /// modeled as independent by ordering/conflict analysis.
    #[test]
    fn chose_other_ring_bearer_is_event_bound() {
        use crate::types::ability::TriggerCondition;

        let axes = scan_trigger_condition(
            &TriggerCondition::ChoseOtherRingBearer,
            ScanMode::Conservative,
        );
        assert!(axes.event, "must be event-bound");
        assert!(!axes.sibling);
        assert!(!axes.projected);
    }

    /// #7816: same event-bound classification as its `ChoseOtherRingBearer`
    /// sibling — chooser and bearer live on the triggering temptation.
    #[test]
    fn chose_ring_bearer_is_event_bound() {
        use crate::types::ability::TriggerCondition;

        let axes =
            scan_trigger_condition(&TriggerCondition::ChoseRingBearer, ScanMode::Conservative);
        assert!(axes.event, "must be event-bound");
        assert!(!axes.sibling);
        assert!(!axes.projected);
    }

    // ═════════════════════════════════════════════════════════════════════════
    // The `TriggerDefinition` walker's verification rows. Each row is paired with a
    // revert probe that must flip it.
    // ═════════════════════════════════════════════════════════════════════════

    /// The NON-DEGENERATE filter value every B-1a/B-1b path is populated with.
    ///
    /// Each of the seven paths must hold a filter that
    /// WOULD self-assert `sibling: true` if that path were scanned — otherwise
    /// "still `sibling == false`" is uninformative for that path (the degenerate
    /// window one abstraction up).
    ///
    /// This value self-asserts in BOTH `FilterReadContext`s, so its non-degeneracy
    /// is a property of the VALUE and not of the context a revert happens to pick:
    /// the `Typed` arm -> `typed_filter_axes` -> `scan_filter_prop` ->
    /// `FilterProp::Targets` -> `scan_target_filter(.., LiveBoardCensus, ..)`,
    /// whose `base` injects `sibling: true`. A bare `Typed{Creature}` would NOT
    /// do: under `LoopFirewall` the `Typed` arm relaxes to `props.sibling`.
    fn census_asserting_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            properties: vec![FilterProp::Targets {
                filter: Box::new(TargetFilter::Any),
            }],
            ..TypedFilter::creature()
        })
    }

    /// NON-DEGENERACY SELF-CHECK (the positive control for all seven paths).
    /// If this ever goes green-by-vacuity the seven population assertions below
    /// are measuring nothing. Asserted in the RELAXED context, which is the strong
    /// direction: `SnapshotOrEvent` gives `base == Axes::NONE`, so a `true` here
    /// can only have come from the filter's own shape.
    #[test]
    fn nondegeneracy_fixture_filter_self_asserts_sibling() {
        assert!(
            scan_target_filter(
                &census_asserting_filter(),
                FilterReadContext::SnapshotOrEvent,
                ScanMode::LoopFirewall,
            )
            .sibling,
            "fixture filter must self-assert sibling from its OWN shape (relaxed ctx)"
        );
        // Control on the shape that would make the population degenerate.
        assert!(
            !scan_target_filter(
                &TargetFilter::Typed(TypedFilter::creature()),
                FilterReadContext::SnapshotOrEvent,
                ScanMode::LoopFirewall,
            )
            .sibling,
            "a bare Typed{{Creature}} must RELAX under LoopFirewall — if it does not, \
             the fixture above proves nothing about FilterProp recursion"
        );
    }

    /// Bello, Bard of the Brambles' granted trigger, verbatim from its Oracle text
    /// ("Whenever this creature deals combat damage to a player, draw a card").
    fn bello_granted_trigger() -> TriggerDefinition {
        TriggerDefinition {
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))),
            valid_source: Some(TargetFilter::SelfRef),
            valid_target: Some(TargetFilter::Player),
            damage_kind: DamageKindFilter::CombatOnly,
            ..TriggerDefinition::new(TriggerMode::DamageDone)
        }
    }

    fn grant_trigger(def: TriggerDefinition) -> ContinuousModification {
        ContinuousModification::GrantTrigger {
            trigger: Box::new(def),
        }
    }

    fn sibling_of(m: &ContinuousModification) -> bool {
        scan_continuous_modification(m, ScanMode::LoopFirewall).sibling
    }

    // ── B-1 (POSITIVE) ──────────────────────────────────────────────────────
    /// B-1: Bello's exact shape clears. Revert probe: return the arm to the
    /// blanket `CONSERVATIVE` group ⇒ this row flips to `sibling == true`.
    #[test]
    fn b1_bello_exact_shape_clears() {
        assert!(
            !sibling_of(&grant_trigger(bello_granted_trigger())),
            "B-1: Bello's granted trigger reads nothing sibling-mutable"
        );
    }

    /// B-1 PAIRED POSITIVE REACH-GUARD. The identical `execute` body routed through
    /// the ALREADY-descending `GrantAbility` arm must already read nothing.
    /// Without this, B-1's green is unattributable: it could mean "the walker
    /// works" or "this Draw body was never capable of reporting an axis". It is
    /// also what makes B-1's pre-mechanism RED gate interpretable.
    #[test]
    fn b1_reach_guard_grant_ability_draw_body_is_clean() {
        let body = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        assert!(
            !sibling_of(&ContinuousModification::GrantAbility {
                definition: Box::new(body),
            }),
            "reach-guard: the Draw body itself is clean through the GrantAbility arm"
        );
    }

    // ── B-1a (POSITIVE ×5) — the five EVENT-MATCHER paths are SCANNED ─────────
    // Each row (i) populates exactly ONE path with the non-degenerate filter,
    // (ii) asserts that population's non-degeneracy inline, (iii) asserts the
    // walker reports the filter's own read. Each has its OWN revert probe.
    // The paired context pin below fixes WHICH `FilterReadContext` they route at.

    macro_rules! nondegenerate {
        ($f:expr) => {
            assert!(
                scan_target_filter(
                    $f,
                    FilterReadContext::SnapshotOrEvent,
                    ScanMode::LoopFirewall
                )
                .sibling,
                "population is DEGENERATE: this filter would not self-assert sibling \
                 even if the path were scanned, so the row below proves nothing"
            );
        };
    }

    /// B-1a path 1 — `valid_card` is a SCANNED matcher surface.
    /// Revert probe: bind `valid_card: _` read-free again ⇒ this row flips to
    /// `sibling == false`.
    #[test]
    fn b1a_p1_valid_card_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            valid_card: Some(f),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1a p1: valid_card is scanned"
        );
    }

    /// B-1a path 2 — `valid_target` is a SCANNED matcher surface.
    /// Revert probe: bind `valid_target: _` read-free again ⇒ this row flips to
    /// `sibling == false`.
    #[test]
    fn b1a_p2_valid_target_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            valid_target: Some(f),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1a p2: valid_target is scanned"
        );
    }

    /// B-1a path 3 — `valid_subject_player` is a SCANNED matcher surface.
    /// Revert probe: bind `valid_subject_player: _` read-free again ⇒ this row flips to
    /// `sibling == false`.
    #[test]
    fn b1a_p3_valid_subject_player_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            valid_subject_player: Some(f),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1a p3: valid_subject_player is scanned"
        );
    }

    /// B-1a path 4 — `valid_source` is a SCANNED matcher surface.
    /// Revert probe: bind `valid_source: _` read-free again ⇒ this row flips to
    /// `sibling == false`.
    #[test]
    fn b1a_p4_valid_source_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            valid_source: Some(f),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1a p4: valid_source is scanned"
        );
    }

    /// B-1a path 5 — `zone_change_clauses` -> `ZoneChangeClause::valid_card` is a
    /// SCANNED matcher surface. Populated with a NON-EMPTY vec whose clause carries
    /// the filter: `vec![]` is the degeneracy the non-degenerate-filter doc above
    /// names explicitly.
    /// Revert probe: drop the clause loop (bind `zone_change_clauses: _`) ⇒ this row
    /// flips to `sibling == false`.
    #[test]
    fn b1a_p5_zone_change_clause_valid_card_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let clause = ZoneChangeClause {
            origin: OriginConstraint::Any,
            destination: Some(Zone::Battlefield),
            destination_constraint: DestinationConstraint::Any,
            valid_card: Some(f),
        };
        let def = TriggerDefinition {
            zone_change_clauses: vec![clause],
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1a p5: clause valid_card is scanned"
        );
        // Guard the vec is actually populated (the named degeneracy trap): the
        // empty-vec form must NOT reach the same verdict, or p5 measures nothing.
        let empty = TriggerDefinition {
            zone_change_clauses: vec![],
            ..bello_granted_trigger()
        };
        assert!(
            !sibling_of(&grant_trigger(empty)),
            "control: the empty-vec form is the DEGENERATE population — it must not \
             be what row p5 is measuring"
        );
    }

    /// B-1a EVENT-AXIS MASK — a matcher's `event` read does not propagate, while its
    /// `sibling` read does. Pinned because `TargetFilter::Typed` sets `event`
    /// unconditionally, so scanning matchers at all would otherwise re-classify every
    /// typed matcher as an ordering-relevant event read.
    ///
    /// REACH-GUARD: the fixture filter's own scan sets `event`, asserted inline — so a
    /// masked `false` below is the mask's verdict and not the filter's silence.
    /// Revert probe: drop the `Axes { event: false, .. }` mask ⇒ **FAILS**.
    #[test]
    fn b1a_matcher_event_axis_is_masked() {
        let f = census_asserting_filter();
        assert!(
            scan_target_filter(
                &f,
                FilterReadContext::SnapshotOrEvent,
                ScanMode::LoopFirewall,
            )
            .event,
            "reach-guard: the fixture filter must set `event` on its OWN scan, or the \
             mask assertion below is vacuous"
        );
        let def = TriggerDefinition {
            valid_card: Some(f),
            ..bello_granted_trigger()
        };
        let axes = scan_trigger_definition(&def, ScanMode::LoopFirewall);
        assert!(
            !axes.event,
            "a matcher selects WHICH event fires (CR 603.2); it is not the \
             resolution-time event read (CR 603.4) the ordering term consumes"
        );
        assert!(
            axes.sibling,
            "paired positive: the SAME matcher's board read still propagates, so the \
             mask above is axis-scoped and not a re-muted path"
        );
    }

    /// B-1a CONTEXT PIN — the five matcher call sites route at `SnapshotOrEvent`,
    /// NOT the `LiveBoardCensus` default. A bare `Typed{Creature}` matcher carries
    /// no board-reading component, so under `LoopFirewall` it must RELAX;
    /// `LiveBoardCensus` would inject `sibling: true` from the call site regardless
    /// of the filter's shape and veto every granted-trigger offer whose matcher
    /// merely names a card type.
    ///
    /// NON-VACUITY: the same five paths carrying `census_asserting_filter()` DO
    /// report `sibling` in rows p1..p5 above, so a green here is the context's
    /// verdict and not an unreachable path.
    /// Revert probe: route any one of the five through `LiveBoardCensus` ⇒ that
    /// path's case FAILS.
    #[test]
    fn b1a_matcher_paths_route_at_snapshot_or_event() {
        let bare = || TargetFilter::Typed(TypedFilter::creature());
        let cases: [(&str, TriggerDefinition); 5] = [
            (
                "valid_card",
                TriggerDefinition {
                    valid_card: Some(bare()),
                    ..bello_granted_trigger()
                },
            ),
            (
                "valid_target",
                TriggerDefinition {
                    valid_target: Some(bare()),
                    ..bello_granted_trigger()
                },
            ),
            (
                "valid_subject_player",
                TriggerDefinition {
                    valid_subject_player: Some(bare()),
                    ..bello_granted_trigger()
                },
            ),
            (
                "valid_source",
                TriggerDefinition {
                    valid_source: Some(bare()),
                    ..bello_granted_trigger()
                },
            ),
            (
                "zone_change_clauses",
                TriggerDefinition {
                    zone_change_clauses: vec![ZoneChangeClause {
                        origin: OriginConstraint::Any,
                        destination: Some(Zone::Battlefield),
                        destination_constraint: DestinationConstraint::Any,
                        valid_card: Some(bare()),
                    }],
                    ..bello_granted_trigger()
                },
            ),
        ];
        for (label, def) in cases {
            assert!(
                !sibling_of(&grant_trigger(def)),
                "{label}: a bare Typed matcher must relax under LoopFirewall — this \
                 path is routed at LiveBoardCensus, which vetoes every typed matcher"
            );
        }
    }

    // ── The `Effect::CreateDelayedTrigger` mode-split descent ────────────────

    /// A `TriggerDefinition` whose `execute` body reads the growing class, composed from
    /// the fixtures above: Bello's shape with a pump whose TARGET self-asserts `sibling`.
    /// Used wherever a delegating arm needs a payload the arm-deletion mutation can reach.
    fn class_reading_trigger() -> TriggerDefinition {
        TriggerDefinition {
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                read_free_pump(census_asserting_filter()),
            ))),
            ..bello_granted_trigger()
        }
    }

    /// A delayed body that reads nothing: two `PtValue::Fixed` halves and a `SelfRef`
    /// target. Every row that uses it asserts its inertness rather than assuming it.
    fn inert_delayed_body() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, read_free_pump(TargetFilter::SelfRef))
    }

    fn delayed_node(condition: DelayedTriggerCondition, body: AbilityDefinition) -> Effect {
        Effect::CreateDelayedTrigger {
            condition,
            effect: Box::new(body),
            uses_tracked_set: false,
        }
    }

    /// Chocobo Camp's REAL parsed `abilities[0]` sub-ability effect — the
    /// `Effect::CreateDelayedTrigger` this arm classifies — from the card's VERBATIM
    /// Oracle text, never a paraphrase.
    fn chocobo_delayed_node() -> Effect {
        let parsed = crate::parser::parse_oracle_text(
            "This land enters tapped unless you control a legendary creature.\n\
             {T}: Add {G}. When you next cast a Bird creature spell this turn, it enters with \
             an additional +1/+1 counter on it.\n\
             {2}{G}{G}, {T}: Create a 2/2 green Bird creature token with \"Whenever a land you \
             control enters, this token gets +1/+0 until end of turn.\"",
            "Chocobo Camp",
            &[],
            &["Land".to_string()],
            &[],
        );
        let sub = parsed.abilities[0]
            .sub_ability
            .as_deref()
            .expect("fixture pin: `abilities[0]` carries the delayed-trigger sub-ability");
        let node = sub.effect.as_ref().clone();
        assert!(
            matches!(node, Effect::CreateDelayedTrigger { .. }),
            "fixture pin: that sub-ability's effect is the `Effect::CreateDelayedTrigger` \
             this arm classifies"
        );
        node
    }

    /// The expected triple per variant, as an EXHAUSTIVE `match` with no `_`: a new
    /// `DelayedTriggerCondition` variant is `E0004` in this test as well as in the scanner,
    /// so the table cannot silently stop covering the enum.
    fn expected_delayed_axes(c: &DelayedTriggerCondition) -> (bool, bool, bool) {
        match c {
            DelayedTriggerCondition::AtNextPhase { .. }
            | DelayedTriggerCondition::AtNextPhaseForPlayer { .. }
            | DelayedTriggerCondition::WhenLeavesPlay { .. } => (false, false, false),
            DelayedTriggerCondition::WhenDies { .. }
            | DelayedTriggerCondition::WhenLeavesPlayFiltered { .. }
            | DelayedTriggerCondition::WhenEntersBattlefield { .. }
            | DelayedTriggerCondition::WhenDiesOrExiled { .. }
            | DelayedTriggerCondition::WheneverEvent { .. }
            | DelayedTriggerCondition::WhenNextEvent { .. } => (true, true, false),
        }
    }

    /// Every `DelayedTriggerCondition` variant is classified, and the classification
    /// DISCRIMINATES: one case per variant, each asserting the exact `Axes` triple.
    ///
    /// Every DELEGATING arm's case carries a payload that scans non-inert, so deleting that
    /// arm's delegation is a real mutation there rather than the identity, and each such case
    /// asserts its own payload's non-degeneracy inline.
    ///
    /// REVERT-PROBE: replace any single arm's body with `Axes::NONE` ⇒ that case FAILS for
    /// every arm whose expected triple is not `Axes::NONE`; replace ALL arms with
    /// `Axes::CONSERVATIVE` ⇒ the inert cases FAIL. Neither a constant-inert nor a
    /// constant-conservative implementation satisfies this row.
    #[test]
    fn delayed_trigger_condition_scanner_classifies_every_variant() {
        use crate::types::phase::Phase;

        let filter = census_asserting_filter();
        nondegenerate!(&filter);
        let payload = class_reading_trigger();
        let payload_axes = scan_trigger_definition(&payload, ScanMode::LoopFirewall);
        assert!(
            payload_axes.sibling,
            "payload DEGENERATE: the trigger fixture the delegating cases carry must scan \
             non-inert through the very function those arms call, or deleting the delegation \
             would be invisible here"
        );

        let cases = [
            (
                "AtNextPhase",
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            ),
            (
                "AtNextPhaseForPlayer",
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::Upkeep,
                    player: PlayerId(0),
                    gate: TurnGate::AfterCreationTurn,
                    binding: crate::types::ability::DelayedTriggerPlayerBinding::Controller,
                },
            ),
            (
                "WhenLeavesPlay",
                DelayedTriggerCondition::WhenLeavesPlay {
                    object_id: ObjectId(7),
                },
            ),
            (
                "WhenDies",
                DelayedTriggerCondition::WhenDies {
                    filter: filter.clone(),
                },
            ),
            (
                "WhenLeavesPlayFiltered",
                DelayedTriggerCondition::WhenLeavesPlayFiltered {
                    filter: filter.clone(),
                },
            ),
            (
                "WhenEntersBattlefield",
                DelayedTriggerCondition::WhenEntersBattlefield {
                    filter: filter.clone(),
                },
            ),
            (
                "WhenDiesOrExiled",
                DelayedTriggerCondition::WhenDiesOrExiled {
                    filter: filter.clone(),
                },
            ),
            (
                "WheneverEvent",
                DelayedTriggerCondition::WheneverEvent {
                    trigger: Box::new(payload.clone()),
                    expiry: WheneverEventExpiry::EndOfTurn,
                },
            ),
            (
                "WhenNextEvent",
                DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(payload.clone()),
                    or_trigger: None,
                    lifetime: DelayedTriggerLifetime::ThisTurn,
                },
            ),
        ];

        for (label, condition) in &cases {
            let axes = scan_delayed_trigger_condition(condition, ScanMode::LoopFirewall);
            assert_eq!(
                (axes.event, axes.sibling, axes.projected),
                expected_delayed_axes(condition),
                "{label}: CR 732.2a — this variant's exact read-axis triple. A constant \
                 implementation in either direction fails some case in this table"
            );
        }
    }

    /// `WhenNextEvent`'s `or_trigger` is SCANNED, not dropped, and the condition leg is
    /// wired into `scan_effect`'s arm.
    ///
    /// The primary `trigger` is inert and the read lives entirely in `or_trigger`, so a
    /// `{ trigger, .. }` destructure — which compiles and passes every variant-coverage
    /// test — reddens exactly here. The delayed body's inertness is asserted in the row,
    /// so the veto is the condition leg's.
    ///
    /// REVERT-PROBE: delete the `if let Some(t) = or_trigger` leg ⇒ **FAILS**; replace the
    /// arm body with `ability_definition_axes(effect, mode)` alone (drop the condition leg)
    /// ⇒ **FAILS**.
    #[test]
    fn when_next_event_scans_the_or_trigger() {
        let body = inert_delayed_body();
        let body_axes = ability_definition_axes(&body, ScanMode::LoopFirewall);
        assert_eq!(
            (body_axes.event, body_axes.sibling, body_axes.projected),
            (false, false, false),
            "reach guard: the delayed body must read nothing, or the veto below could be \
             the body leg's rather than the condition leg's"
        );

        let alt = class_reading_trigger();
        assert!(
            scan_trigger_definition(&alt, ScanMode::LoopFirewall).sibling,
            "population is DEGENERATE: the alternate matcher must itself read the growing \
             class, or a dropped `or_trigger` leg would be invisible here"
        );

        let node = delayed_node(
            DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(bello_granted_trigger()),
                or_trigger: Some(Box::new(alt)),
                lifetime: DelayedTriggerLifetime::ThisTurn,
            },
            body.clone(),
        );
        assert!(
            scan_effect(&node, ScanMode::LoopFirewall).sibling,
            "CR 732.2a: \"when you next [event] OR [event]\" carries TWO matchers of equal \
             authority. Classifying only the first lets a board-reading alternate matcher \
             through the object-growth firewall"
        );

        // PAIRED POSITIVE: the same inert primary matcher with no alternate relieves, so
        // this row is not a blanket veto on `WhenNextEvent`.
        let relieved = delayed_node(
            DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(bello_granted_trigger()),
                or_trigger: None,
                lifetime: DelayedTriggerLifetime::ThisTurn,
            },
            body,
        );
        let axes = scan_effect(&relieved, ScanMode::LoopFirewall);
        assert_eq!(
            (axes.event, axes.sibling, axes.projected),
            (false, false, false),
            "paired positive: with the alternate matcher gone the identical shape is \
             relieved, so the veto above is the `or_trigger` leg's"
        );
    }

    /// A filter-carrying delayed condition fails CLOSED on all four variants, and the
    /// `sibling` half comes from the census CONTEXT rather than from the filter's own shape.
    ///
    /// The second fixture is a SINGLE-OBJECT reference (`TargetFilter::ParentTarget`), which
    /// scans `(true, false, false)` on its own — it yields the same triple here only because
    /// the arm passes `LiveBoardCensus`, which is what makes the attribution visible.
    ///
    /// REVERT-PROBE: swap `FilterReadContext::LiveBoardCensus` for `SnapshotOrEvent` in the
    /// four-variant arm ⇒ **FAILS**; replace the arm body with
    /// `ability_definition_axes(effect, mode)` alone (drop the condition leg) ⇒ **FAILS**.
    #[test]
    fn filter_carrying_delayed_conditions_fail_closed() {
        use crate::types::phase::Phase;

        let body = inert_delayed_body();
        let body_axes = ability_definition_axes(&body, ScanMode::LoopFirewall);
        assert_eq!(
            (body_axes.event, body_axes.sibling, body_axes.projected),
            (false, false, false),
            "reach guard: the delayed body must read nothing, so each triple below is the \
             condition's"
        );

        let bare = TargetFilter::Typed(TypedFilter::creature());
        let single_object = TargetFilter::ParentTarget;
        for (label, filter) in [("bare Typed", &bare), ("ParentTarget", &single_object)] {
            let built: [(&str, DelayedTriggerCondition); 4] = [
                (
                    "WhenDies",
                    DelayedTriggerCondition::WhenDies {
                        filter: filter.clone(),
                    },
                ),
                (
                    "WhenLeavesPlayFiltered",
                    DelayedTriggerCondition::WhenLeavesPlayFiltered {
                        filter: filter.clone(),
                    },
                ),
                (
                    "WhenEntersBattlefield",
                    DelayedTriggerCondition::WhenEntersBattlefield {
                        filter: filter.clone(),
                    },
                ),
                (
                    "WhenDiesOrExiled",
                    DelayedTriggerCondition::WhenDiesOrExiled {
                        filter: filter.clone(),
                    },
                ),
            ];
            for (variant, condition) in built {
                let axes = scan_effect(
                    &delayed_node(condition, body.clone()),
                    ScanMode::LoopFirewall,
                );
                assert_eq!(
                    (axes.event, axes.sibling, axes.projected),
                    (true, true, false),
                    "{variant} / {label}: the matcher filter has no owning authority to \
                     delegate to, so it is read under `LiveBoardCensus` and the `sibling` \
                     half is the census's own — precise, not a blanket, since `projected` \
                     stays false"
                );
            }
        }

        // PAIRED POSITIVE: a payload-free coordinate on the same shape relieves, so the arm
        // is not a blanket veto on every delayed condition.
        let axes = scan_effect(
            &delayed_node(
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                body,
            ),
            ScanMode::LoopFirewall,
        );
        assert_eq!(
            (axes.event, axes.sibling, axes.projected),
            (false, false, false),
            "paired positive: a phase coordinate reaches no filter, so the identical node \
             shape is relieved"
        );
    }

    /// `uses_tracked_set: true` fails CLOSED.
    ///
    /// The flag resolves the delayed body against the PARENT ability's tracked object set —
    /// a board-dependent referent this definition cannot see — so nothing here can classify
    /// it. One field differs between the two halves of this row.
    ///
    /// REVERT-PROBE: delete the `if *uses_tracked_set` guard ⇒ the flipped node is relieved
    /// ⇒ **FAILS**.
    #[test]
    fn create_delayed_trigger_with_tracked_set_fails_closed() {
        let mut flipped = chocobo_delayed_node();
        let Effect::CreateDelayedTrigger {
            uses_tracked_set, ..
        } = &mut flipped
        else {
            panic!("fixture pin: the parsed sub-ability effect is a `CreateDelayedTrigger`");
        };
        *uses_tracked_set = true;

        let axes = scan_effect(&flipped, ScanMode::LoopFirewall);
        assert_eq!(
            (axes.event, axes.sibling, axes.projected),
            (true, true, true),
            "CR 732.2a: the tracked object set belongs to the PARENT ability's resolution, \
             so this definition carries no way to classify what the body will resolve \
             against. The unclassifiable read surface fails closed"
        );

        // PAIRED POSITIVE: the same printed definition without the flag is relieved, so the
        // veto above is the flag's and not the payload's.
        let printed = scan_effect(&chocobo_delayed_node(), ScanMode::LoopFirewall);
        assert_eq!(
            (printed.event, printed.sibling, printed.projected),
            (false, false, false),
            "paired positive: one field apart, the printed definition is relieved"
        );
    }

    /// `ScanMode::Conservative` is byte-identical for this arm too.
    ///
    /// REVERT-PROBE: the naive revert does NOT compile — deleting the
    /// `ScanMode::Conservative` arm is `E0004`. The discriminating mutation is making the
    /// `Conservative` arm run the `LoopFirewall` descent ⇒ **FAILS**.
    #[test]
    fn scan_effect_create_delayed_trigger_stays_conservative() {
        let filter_bearing = delayed_node(
            DelayedTriggerCondition::WhenDies {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            },
            inert_delayed_body(),
        );
        for (label, node) in [
            ("printed card", chocobo_delayed_node()),
            ("filter-bearing", filter_bearing),
        ] {
            let axes = scan_effect(&node, ScanMode::Conservative);
            assert_eq!(
                (axes.event, axes.sibling, axes.projected),
                (true, true, true),
                "{label}: CR 603.3b — under `Conservative` this arm must stay the \
                 byte-identical fail-closed blanket every non-firewall consumer already \
                 sees. Only the CR 732.2a firewall may observe the descent"
            );
        }

        // PAIRED POSITIVE: the same printed definition under `LoopFirewall` descends, so the
        // row above is byte-identity and not an assertion that nothing ever descends.
        let firewall = scan_effect(&chocobo_delayed_node(), ScanMode::LoopFirewall);
        assert_eq!(
            (firewall.event, firewall.sibling, firewall.projected),
            (false, false, false),
            "paired positive: the identical node descends under `LoopFirewall`"
        );
    }

    /// The arm is reached through the SECOND carrier — a granted trigger whose `execute`
    /// carries the delayed node — and a PROJECTED-ONLY verdict survives to the entry point
    /// that can see it.
    ///
    /// The two continuous-modification entry points read ONE axis each and the disjunction
    /// is their caller's, so a `{sibling: false, projected: true}` verdict is invisible to
    /// one of them by construction. The condition is the inert `AtNextPhase`, which makes
    /// the whole verdict the body leg's.
    ///
    /// REVERT-PROBE: restore `Effect::CreateDelayedTrigger { .. } => Axes::CONSERVATIVE` ⇒
    /// the `sibling == false` assertion **FAILS**, because the blanket sets all three axes;
    /// delete `acc = acc.or(ability_definition_axes(effect, mode))` (drop the body leg) ⇒
    /// the `projected == true` assertion **FAILS**.
    #[test]
    fn grant_trigger_carrying_a_delayed_body_is_descended_per_axis() {
        use crate::types::phase::Phase;

        let projected_half = PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::Controller,
            },
        });
        let projected_body = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: projected_half.clone(),
                toughness: projected_half,
                target: TargetFilter::SelfRef,
            },
        );

        // AXIS ISOLATION, as a reach-guard: if the body stops being projected-ONLY the split
        // this row is about stops being under test.
        let body_axes = ability_definition_axes(&projected_body, ScanMode::LoopFirewall);
        assert_eq!(
            (body_axes.event, body_axes.sibling, body_axes.projected),
            (false, false, true),
            "AXIS ISOLATION: the delayed body must be projected-ONLY. The `sibling: false` \
             half is what makes it invisible to a `.sibling`-only consult"
        );

        let carrier = |body: AbilityDefinition| {
            grant_trigger(TriggerDefinition {
                execute: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    delayed_node(
                        DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                        body,
                    ),
                ))),
                ..bello_granted_trigger()
            })
        };

        let m = carrier(projected_body);
        assert!(
            !continuous_modification_reads_sibling_mutable(&m),
            "CR 732.2a: the delayed body reads a projected player resource and no board \
             aggregate, so the `.sibling` entry point must report the relief this descent \
             delivers on the granted-trigger carrier"
        );
        assert!(
            continuous_modification_reads_projected_resource(&m),
            "CR 608.2h + CR 732.2a: the veto is still reachable, through the OTHER \
             single-axis entry point. Without this half, the assertion above would be \
             satisfied by an arm that classified everything inert"
        );

        // NON-DEGENERACY CONTROL: the same carrier with a read-free delayed body must relieve
        // BOTH entry points, so the `projected` verdict above is the body's and not the
        // carrier's or the trigger's.
        let inert = carrier(inert_delayed_body());
        assert!(
            !continuous_modification_reads_sibling_mutable(&inert)
                && !continuous_modification_reads_projected_resource(&inert),
            "control: with a read-free delayed body the identical carrier reads neither \
             axis, so the `projected` reading above is attributable to the body"
        );
    }

    // ── B-1b (POSITIVE ×3) — the two NON-MATCHER paths are FAIL-CLOSED ────────
    // These paths are scanned, not read-free: no firewall surface reads them
    // (`analysis::resource` reads only `condition` and `execute`), and
    // `ability_definition_axes` already binds the SAME `UnlessPayModifier` type
    // fail-closed. Their reverts flip the OTHER way —
    // binding the path `_` read-free must turn these red.

    /// B-1b path 6 — `constraint` -> `NthSpellThisTurn { filter }`.
    /// Revert probe: bind `constraint: _` read-free ⇒ this row flips to false.
    #[test]
    fn b1b_p6_constraint_filter_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            constraint: Some(TriggerConstraint::NthSpellThisTurn {
                n: 1,
                comparator: Comparator::EQ,
                filter: Some(f),
            }),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1b p6: a filtered spell-count constraint is a scanned read surface"
        );
        // PRECISION control: an unfiltered constraint must NOT veto. This is what
        // distinguishes descending `constraint` from a blanket
        // `constraint.is_some() => CONSERVATIVE`, and `OncePerTurn` is exactly the
        // populated-but-filterless variant.
        let once = TriggerDefinition {
            constraint: Some(TriggerConstraint::OncePerTurn),
            ..bello_granted_trigger()
        };
        assert!(
            !sibling_of(&grant_trigger(once)),
            "B-1b p6 precision: OncePerTurn carries no filter and must not veto"
        );
    }

    /// B-1b path 7a — `unless_pay.payer`.
    /// Revert probe: drop the `payer` delegation ⇒ this row flips to false.
    #[test]
    fn b1b_p7a_unless_pay_payer_is_scanned() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            unless_pay: Some(UnlessPayModifier {
                // Read-free cost, so this row isolates the `payer` sub-surface.
                cost: AbilityCost::Tap,
                payer: f,
            }),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1b p7a: unless_pay.payer is a scanned filter surface"
        );
    }

    /// B-1b path 7b — `unless_pay.cost`. Guarded independently of `payer`
    /// because they are separate delegations that can be dropped separately.
    /// Revert probe: drop the `scan_ability_cost` delegation ⇒ this row flips.
    #[test]
    fn b1b_p7b_unless_pay_cost_is_scanned() {
        let def = TriggerDefinition {
            unless_pay: Some(UnlessPayModifier {
                cost: AbilityCost::ManaDynamic {
                    quantity: object_count(),
                },
                // Read-free payer, so this row isolates the `cost` sub-surface.
                payer: TargetFilter::Controller,
            }),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-1b p7b: a board-dynamic unless-pay cost is a scanned read surface"
        );
    }

    // ── B-2 / B-3 / B-4 (NEGATIVE) ────────────────────────────────────────────

    /// B-2 (MANDATORY): the walker is PRECISE, not permissive — it tests whether
    /// the walker DESCENDS AT ALL. Discriminator: `QuantityRef::ObjectCount`
    /// self-asserts `sibling: true` unconditionally. This is NOT a
    /// filter-precision test.
    /// Revert probe: replace the walker body with `Axes::NONE` ⇒ this row flips.
    #[test]
    fn b2_walker_descends_execute_object_count() {
        let def = TriggerDefinition {
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: object_count(),
                    target: TargetFilter::Controller,
                },
            ))),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-2: a granted execute reading a board ObjectCount must still veto"
        );
    }

    /// B-3: the `condition` surface too. Discriminator is
    /// `TriggerCondition::ControlsType`'s OWN `sibling: true` — deliberately NOT
    /// `ObjectCount`'s, so B-2 and B-3 fail independently: routing both through the
    /// same self-assertion would let one edit red both and make neither attributable.
    /// Revert probe: drop the `scan_trigger_condition` delegation ⇒ this row flips.
    #[test]
    fn b3_walker_descends_condition() {
        let def = TriggerDefinition {
            condition: Some(TriggerCondition::ControlsType {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            }),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-3: a granted condition reading the board must still veto"
        );
    }

    /// B-4: the read-free binding is JUSTIFIED, not blanket. The SAME filter value
    /// sits in two positions and gets OPPOSITE verdicts: on `execute`'s target it
    /// is a read (reached via the `FilterProp` recursion); on the matcher fields it
    /// is read-free. One fixture, both halves — a blanket read-free binding of
    /// `execute`/`condition` cannot satisfy it, and neither can scanning matchers.
    /// Revert probe: bind `execute`/`condition` `_` read-free alongside the matchers
    /// ⇒ this row flips to false.
    #[test]
    fn b4_execute_census_scanned_while_matchers_stay_read_free() {
        let f = census_asserting_filter();
        nondegenerate!(&f);
        let def = TriggerDefinition {
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: f.clone(),
                },
            ))),
            valid_card: Some(f.clone()),
            valid_target: Some(f.clone()),
            valid_subject_player: Some(f.clone()),
            valid_source: Some(f),
            ..bello_granted_trigger()
        };
        assert!(
            sibling_of(&grant_trigger(def)),
            "B-4: the identical filter is a READ on execute's target while staying \
             read-free on all four matcher fields"
        );
    }

    // ── B-5 (scope guard) ─────────────────────────────────────────────────────
    /// B-5: the other blanket-arm members are untouched.
    ///
    /// Every member but `CopyValues` is exercised. That omission is deliberate:
    /// `CopyValues` needs a `CopiableValues` literal (`ManaCost`, `CardType`,
    /// `PrintedLoyalty`, three `Arc<Vec<_>>`) plus a `DisplaySource` from
    /// `game::game_object` — no `Default`, no constructor. The residual risk is
    /// small: `scan_continuous_modification` is exhaustive with NO `_` wildcard, so a
    /// member cannot silently LEAVE the match; only a deliberate move into another
    /// arm would evade this row, and that is visible in a diff.
    /// No revert probe (a scope guard, not a mechanism claim).
    #[test]
    fn b5_other_blanket_members_stay_conservative() {
        let survivors: Vec<ContinuousModification> = vec![
            ContinuousModification::CopyChosen,
            ContinuousModification::RetainAllOtherAbilitiesFromSource,
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::Any,
                cap: None,
            },
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::Any,
            },
            ContinuousModification::RetainPrintedTriggerFromSource {
                source_trigger_index: 0,
            },
            ContinuousModification::RetainPrintedAbilityFromSource {
                source_ability_index: 0,
            },
            ContinuousModification::AddStaticMode {
                mode: StaticMode::Continuous,
            },
            ContinuousModification::GrantStaticAbility {
                definition: Box::new(StaticDefinition::continuous()),
            },
            ContinuousModification::AddKeywordWithDerivedCost {
                kind: CostBearingKeywordKind::Foretell,
                derivation: CostDerivation::ManaCostReducedBy(ManaCost::default()),
            },
            ContinuousModification::GrantReplacement {
                replacement: Box::new(ReplacementDefinition::new(ReplacementEvent::Destroy)),
            },
        ];
        assert_eq!(
            survivors.len(),
            10,
            "scope guard covers 10 of the arm's 11 members; CopyValues is the \
             documented omission"
        );
        for m in &survivors {
            let axes = scan_continuous_modification(m, ScanMode::LoopFirewall);
            assert!(
                axes.event && axes.sibling && axes.projected,
                "B-5: blanket member must stay fail-closed CONSERVATIVE"
            );
        }
    }

    /// A `Pump` whose `target` is the effect's own field — the ONLY shape
    /// [`effect_target_reads_growing_class_for_loop`] is contracted to accept.
    fn pump_with_target(target: TargetFilter) -> Effect {
        Effect::Pump {
            power: crate::types::ability::PtValue::Fixed(1),
            toughness: crate::types::ability::PtValue::Fixed(1),
            target,
        }
    }

    /// The contracted call shape passes the binding assert and returns a
    /// verdict. Without this row the `#[should_panic]` sibling below is satisfiable by an
    /// assert that fires on EVERYTHING, which would be a debug-build outage rather than a
    /// binding.
    #[test]
    fn effect_target_wrapper_accepts_the_effects_own_target_field() {
        let effect = pump_with_target(TargetFilter::SelfRef);
        let Effect::Pump { target, .. } = &effect else {
            unreachable!("built as Pump")
        };
        assert!(
            !effect_target_reads_growing_class_for_loop(&effect, target),
            "`SelfRef` reads no board population, so the contracted shape must answer false \
             — and must not trip the binding assert on its way there"
        );
    }

    /// The doc's "`target` MUST be a target-filter field of `effect`" is now BOUND,
    /// not requested. The wrapper derives its `FilterReadContext` from `effect` via
    /// `effect_target_ctx`, so a `target` belonging to some other effect is answered under
    /// the wrong census discipline — silently, and with a plausible-looking bool.
    ///
    /// MUTATION PROBE: delete the `debug_assert!(effect.target_filter() == Some(target))`
    /// from [`effect_target_reads_growing_class_for_loop`] ⇒ this row FAILS (no panic).
    #[test]
    #[should_panic(expected = "must BE that effect's target filter")]
    fn effect_target_wrapper_refuses_a_target_that_is_not_the_effects_own() {
        let effect = pump_with_target(TargetFilter::SelfRef);
        // A filter that is NOT `effect`'s field. `Effect::target_filter()` is the authority
        // that says so, and it is what the assert consults.
        let foreign = TargetFilter::Any;
        let _ = effect_target_reads_growing_class_for_loop(&effect, &foreign);
    }
}

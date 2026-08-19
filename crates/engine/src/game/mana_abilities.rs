use crate::game::functioning_abilities::static_kind_present;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, CardSelectionMode, ChoiceValue,
    ChosenAttribute, ContinuousModification, CostPaidObjectSnapshot, Effect, ManaProduction,
    QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, REMOVE_COUNTER_COST_ALL,
    REMOVE_COUNTER_COST_ANY_NUMBER,
};
use crate::types::ability_visit::{
    visit_ability_def_costs_scoped, visit_ability_def_scoped, ResolutionScope,
};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    CostResume, GameState, ManaAbilityCostCursor, ManaAbilityCostParent,
    ManaAbilityCostParentLifecycle, ManaAbilityCostResolutionMode, ManaAbilityResume, ManaChoice,
    ManaChoiceContext, ManaChoicePrompt, ManaColorChoiceResume, ManaTriggerFixedPointResume,
    PayCostKind, PayableResource, PendingCostMoveResume, PendingManaAbility, ProductionOverride,
    WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost, ManaPool, ManaType, PaymentContext};
#[cfg(test)]
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticModeKind;
use crate::types::zones::Zone;
use std::collections::HashSet;
use std::ops::ControlFlow;

use super::cost_payability::{eligible_exile_cost_objects, exile_cost_effective_zone};
use super::effects::mana::resolve_restrictions;
use super::engine::EngineError;
use super::filter::{matches_target_filter, FilterContext};
use super::life_costs::{self, PayLifeCostResult};
use super::mana_payment;
use super::mana_sources;
use super::mana_sources::{mana_color_to_type, mana_type_to_color};
use super::sacrifice;
use super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};

/// CR 605.1a, criteria (1)-(3) ONLY — no target (CR 115.6), the root effect adds
/// mana, and it's not a loyalty ability (CR 606.2). Deliberately EXCLUDES the
/// fourth criterion ("its cost and effect don't move any card to or from a
/// library"), which is why this is NOT the mana-ability test and must never be
/// used for activation routing — use [`is_mana_ability`] for that.
///
/// This exists because [`is_renewable_mana_ability`] asks a different question:
/// "is this permanent part of a standing manabase?" A Millikin
/// ("{T}, Mill a card: Add {C}") stops being a rules mana ability under the
/// library clause but does not stop being a manabase permanent. Composing the
/// development predicate on the rules predicate would delete Millikin, Deranged
/// Assistant, and Codie from `phase-ai`'s `is_intrinsic_mana_source` ->
/// `card_value::mana_role` -> mulligan `keep_tier`, for a reason unrelated to
/// manabase development.
///
/// CR 605.3b: Mana abilities produce mana and resolve immediately without using
/// the stack.
/// CR 605.1a: A mana ability cannot have targets. `Effect::Mana` carries a
/// `ManaTargetRole` naming its recipient and/or count-source player targets;
/// any declared role means the ability targets and must use the stack. The
/// `multi_target` mechanism is checked alongside it.
fn produces_mana_on_activation(ability_def: &AbilityDefinition) -> bool {
    // CR 605.1a: A mana ability "doesn't require a target." Read the ROLE's
    // declared filters: ANY declared role — recipient or count source — means
    // the ability names a target and therefore uses the stack (Jeska's Will
    // mode 1: "Add {R} for each card in target opponent's hand").
    // `declared_filters`, not `surfaced_filters`: a context-ref recipient still
    // makes this not-a-mana-ability under today's behavior, and this change
    // must not widen mana-ability status for any shipping card.
    let target_attached = match &*ability_def.effect {
        Effect::Mana { target, .. } => target.as_ref().and_then(|r| r.declared_filters().next()),
        _ => return false,
    };
    // CR 605.1a: A targeted mana-producing ability is not a mana ability.
    // Reject both the explicit `multi_target` mechanism and the embedded
    // `Effect::Mana::target` field (Jeska's Will mode 1: "Add {R} for each
    // card in target opponent's hand" — the spell targets, so it must use the
    // stack and is not a mana ability under CR 605).
    if ability_def.multi_target.is_some() || target_attached.is_some() {
        return false;
    }
    // CR 605.1a: "...and it's not a loyalty ability." A loyalty ability (CR 606)
    // that happens to add mana — e.g. Chandra, Bold Pyromancer's `[+1]: Add
    // {R}{R}` — is NOT a mana ability: it uses the stack and obeys loyalty-ability
    // timing (CR 606.3, sorcery speed, once per turn). Excluding it here keeps it
    // off the instant-speed mana-ability path.
    // CR 606: a loyalty ability adjusts loyalty as its cost — exclude it here.
    if mana_sources::cost_has_component(&ability_def.cost, |c| {
        matches!(c, AbilityCost::Loyalty { .. })
    }) {
        return false;
    }
    true
}

/// CR 605.1a + CR 608.2c: does any effect this ability executes during its OWN
/// resolution move a card to or from a library? Walks the head effect, the
/// cost's embedded effects, and the `sub_ability` / `else_ability` /
/// `mode_abilities` chain, stopping at the CR 603.3 boundary owned by
/// [`ResolutionScope::OwnResolutionOnly`] — so a payload that is merely
/// *registered* to resolve later (a CR 603.7a delayed trigger, a CR 603.12
/// reflexive trigger, a CR 614.1 replacement, an emblem, a token's granted
/// abilities) is not attributed to this ability.
fn chain_moves_card_to_or_from_library(ability_def: &AbilityDefinition) -> bool {
    visit_ability_def_scoped(
        ability_def,
        ResolutionScope::OwnResolutionOnly,
        &mut |effect| {
            if effect.moves_card_to_or_from_library() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    )
    .is_break()
}

/// CR 605.1a "its cost": the root activation cost (CR 602.1a — "the activation
/// cost is everything before the colon"), PLUS every cost paid during this
/// ability's own resolution, which CR 118.12a -> CR 118.12 classifies as a cost
/// ("the action [do something] is a cost, paid when the spell or ability
/// resolves") and CR 608.2c therefore places under "its effect":
/// `unless_pay.cost` and the `cost` on every `sub_ability` / `else_ability` /
/// `mode_abilities` link.
///
/// This CANNOT be folded into [`chain_moves_card_to_or_from_library`]: that
/// walk's visitor is `FnMut(&Effect)`, and `AbilityCost::Mill` / `Exile` /
/// `ExileWithAggregate` / `ReturnToHand` carry no nested `Effect` at all, so they
/// are structurally invisible to it. That is a type-level gap, not a missing
/// match arm — see `ability_visit::visit_ability_def_costs_scoped`.
fn cost_moves_card_to_or_from_library(ability_def: &AbilityDefinition) -> bool {
    visit_ability_def_costs_scoped(
        ability_def,
        ResolutionScope::OwnResolutionOnly,
        &mut |cost| {
            if cost.moves_card_to_or_from_library() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    )
    .is_break()
}

/// CR 605.1a: the single authority for "is this activated ability a mana
/// ability?" — all four criteria.
///
/// CR 605.1a (final sentence): "Do not take into account replacement effects
/// that may apply, other than self-replacement effects, when evaluating these
/// criteria." This function is a pure function of the printed
/// `AbilityDefinition` AST — it takes no `&GameState` and therefore CANNOT
/// observe a replacement effect. That purity IS the implementation of the
/// clause, not an accident of the signature: do NOT add a `&GameState`
/// parameter or consult the replacement registry here. Self-replacement effects
/// (CR 614.15), which the rule DOES admit, are printed on the ability itself and
/// so are already in the AST this function reads — see the
/// `Effect::Counter { countered_spell_zone }` arm of
/// `Effect::moves_card_to_or_from_library`, which counts Memory Lapse's
/// "instead" precisely because it is a self-replacement effect.
///
/// CR 605.2 is the second reason the signature must stay pure: "A mana ability
/// remains a mana ability even if the game state doesn't allow it to produce
/// mana." A classification that could read game state would invite exactly the
/// state-dependent answer CR 605.2 forbids. (This is also why `Effect::Dig` is
/// unconditionally true: its only non-moving configuration is state-dependent.)
pub fn is_mana_ability(ability_def: &AbilityDefinition) -> bool {
    produces_mana_on_activation(ability_def)
        // CR 605.1a: "...and its cost and effect don't move any card to or from
        // a library." Chromatic Sphere ("{1}, {T}, Sacrifice this artifact: Add
        // one mana of any color. Draw a card.") is the canonical effect case;
        // Millikin ("{T}, Mill a card: Add {C}") is the cost-side case.
        //
        // The cost axis runs off its OWN walk, not the root cost alone: CR
        // 602.1a scopes "its cost" to the activation cost, but CR 118.12a ->
        // CR 118.12 makes an `unless [player] pays` action a cost paid AT
        // RESOLUTION, which CR 608.2c places under "its effect" — as are the
        // costs on chain links.
        //
        // NOT reclassified, and each for a different CR reason:
        //  - Chromatic Star  — the draw is a separate ChangesZone trigger.
        //  - Barbed Sextant  — CR 603.7a, a delayed triggered ability.
        //  - Shaun & Rebecca — CR 603.12, a reflexive triggered ability.
        //  - Gilanra         — CR 603.3, a TriggerOnSpend mana-spend grant.
        //  - The Secret Lair — CR 701.22a, Scry reorders WITHIN a library.
        && !cost_moves_card_to_or_from_library(ability_def)
        && !chain_moves_card_to_or_from_library(ability_def)
}

/// CR 701.21a: Detects when this ability's cost sacrifices **the source itself**.
///
/// Also detects a self-`ReturnToHand` cost: either form removes the source from
/// the battlefield. Restricted to [`TargetFilter::SelfRef`] on purpose: a
/// sac-outlet mana ability that eats *other* permanents (Skirk Prospector,
/// "Sacrifice a Goblin: Add {R}") keeps its source on the battlefield and stays
/// renewable. Delegates to [`mana_sources::cost_has_component`] so a **bare**
/// component and one nested in a `Composite` are both matched.
///
/// Deliberately private, but privacy is a signpost rather than a barrier: it
/// removes the *convenient* route to the naive per-permanent form ("has a mana
/// ability AND no mana ability self-sacs" — which wrongly drops a permanent
/// carrying both a renewable and a self-sacrificing mana ability), and does not
/// make that form inexpressible. `AbilityCost::Sacrifice` and
/// `TargetFilter::SelfRef` are both public, so any downstream crate can rebuild
/// this predicate, and one did: `phase-ai` carried a hand-rolled
/// `ability_cost_requires_sacrifice` whose own doc said it "mirrors
/// `mana_sources::cost_requires_sacrifice` which is private to the engine
/// module". It matched only the `Composite` shape, so Gold's **bare**
/// `Sacrifice` hit its `_ => false` arm and a Gold token counted as renewable.
/// That miscount — a re-implementation drifting from the rule — is the argument
/// for keeping the classification here, not any guarantee privacy provides. The
/// only exported composition is `.any(is_renewable_mana_ability)`, which *is*
/// the per-ability filter.
fn cost_removes_self_from_battlefield(cost: &Option<AbilityCost>) -> bool {
    mana_sources::cost_has_component(cost, |c| {
        matches!(
            c,
            AbilityCost::Sacrifice(s) if s.target == TargetFilter::SelfRef
        ) || matches!(
            c,
            AbilityCost::ReturnToHand {
                filter: Some(TargetFilter::SelfRef),
                ..
            }
        )
    })
}

/// CR 605.1a criteria (1)-(3) + CR 701.21: a *renewable* mana ability — one that
/// produces mana (per [`produces_mana_on_activation`]) without consuming its own
/// source to do it.
///
/// This is the **development** predicate: it answers "is this permanent part of a
/// standing manabase," not "can this produce mana right now." A Treasure, Gold,
/// Lotus Petal, Black Lotus, or Chromatic Star is a one-shot conversion of a
/// permanent into mana and is deliberately **excluded**; Commander's Sphere,
/// Powerstone, Springleaf Drum, and Skirk Prospector are **included**.
///
/// For live availability use [`is_mana_ability`] directly — an untapped Treasure
/// genuinely is one mana available right now, which is why the two predicates must
/// not be unified.
///
/// **DELIBERATELY COMPOSED ON [`produces_mana_on_activation`], NOT ON
/// [`is_mana_ability`] — do not "simplify" this back.** The two predicates answer
/// different questions, so the CR 605.1a library criterion (criterion 4) must NOT
/// reach this one. A **Millikin** or **Deranged Assistant** (`{T}, Mill a card:
/// Add {C}`) and **Codie, Vociferous Codex** stop being rules mana abilities
/// under the library clause, but they do not stop being manabase permanents:
/// they still turn a tap into mana every turn without consuming themselves.
/// Composing the development predicate on the rules predicate would demote all
/// three to `ManaRole::None` through `phase-ai`'s `is_intrinsic_mana_source` ->
/// `card_value::mana_role` -> `plan::controlled_mana_sources`, deleting them from
/// manabase development and from the `mana_behind` deficit that drives mulligan
/// `keep_tier` — an AI-strength regression for a reason that has nothing to do
/// with manabase development. This composition keeps the value **unchanged for
/// every input** across the CR 605.1a amendment.
///
/// Takes a single ability so callers compose with `.any()`; a permanent counts if
/// **at least one** of its mana abilities is renewable (Crystal Vein carries both
/// a renewable `{T}: Add {C}` and a self-sac `{T}, Sac: Add {C}{C}`).
pub fn is_renewable_mana_ability(ability_def: &AbilityDefinition) -> bool {
    produces_mana_on_activation(ability_def)
        && !cost_removes_self_from_battlefield(&ability_def.cost)
}

/// CR 605.1b: A triggered ability is a mana ability iff all three hold:
///   (a) it doesn't require a target (CR 115.6),
///   (b) it triggers from the activation/resolution of an activated mana ability
///       OR from mana being added to a player's mana pool,
///   (c) it could add mana to a player's mana pool when it resolves.
///
/// Triggered mana abilities don't use the stack (CR 605.3b applies analogously);
/// they resolve immediately at the moment the trigger event occurs. This is the
/// single authority for classifying triggered mana abilities — all trigger-enqueue
/// call sites must route through this classifier.
///
/// `trigger_event` is the event that caused the trigger to fire (CR 603.7c).
///
/// Criterion (c) requires that **every** reachable link in the resolution graph
/// (the `sub_ability` chain and the `else_ability` branch at each link, per
/// CR 608.2c) is `Effect::Mana`. Inline resolution runs the full chain without
/// giving any player priority — so a mixed chain like "add {G}, then draw a
/// card" must use the stack, not route inline. "Any link adds mana" is too
/// permissive: it would skip priority on the draw.
///
/// Criterion (b) accepts `TappedForMana` (CR 106.12a) — the per-resolution
/// event emitted whenever a `{T}`-cost mana ability resolves and produces mana,
/// which is exactly the event a `TapsForMana` triggered mana ability fires
/// from. It also accepts `ManaAdded`, because CR 605.1b explicitly includes
/// abilities that trigger from mana being added. CR 605.1b also admits
/// "triggered from the activation/resolution of an activated mana ability" in
/// general, but mana abilities bypass the stack and do not emit a
/// distinguishable `AbilityActivated` event; widening (b) to that axis requires
/// first emitting such an event. No real card exercises the gap today.
pub fn is_triggered_mana_ability(
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
) -> bool {
    // (c) Every reachable link must produce mana. A mixed chain (Mana + Draw,
    // Mana + Damage, …) cannot route inline because non-mana effects in the
    // chain require stack resolution to give players priority.
    if !chain_is_all_mana(ability) {
        return false;
    }
    // (a) No target anywhere in the reachable resolution graph — mirrors the
    // activated-mana-ability guard in `is_mana_ability`. A downstream link
    // with targets (CR 115.6) disqualifies inline resolution, since the full
    // chain must resolve without interrupting for target selection.
    if chain_has_any_targets(ability) {
        return false;
    }
    // (b) CR 106.12a / CR 605.1b: triggered by a `{T}`-cost mana ability
    // resolving and producing mana, or by mana being added. See the doc comment
    // above for the deliberately-not-yet-widened `AbilityActivated` axis.
    matches!(
        trigger_event,
        Some(
            GameEvent::TappedForMana { .. }
                | GameEvent::ManaAbilityProduced { .. }
                | GameEvent::ManaAdded { .. }
        )
    )
}

/// CR 605.1b + CR 605.4a: the **resolver-facing** counterpart of
/// [`is_triggered_mana_ability`].
///
/// [`is_triggered_mana_ability`] is the *acceptance-time* gate: it answers
/// "does this classification-time graph qualify?" and is deliberately raw —
/// any target anywhere in the graph makes it false. That is the right question
/// once, when an occurrence is accepted.
///
/// It is the wrong question during resolution. `resolve_ability_chain` may
/// materialize an engine resolution-context referent (a `chosen_players`
/// member surfacing through `ControllerRef::ChosenPlayer`, for instance) into
/// the overloaded `ResolvedAbility.targets` vector. That referent is not a
/// CR 115.1d announcement target — `build_target_slots` surfaces no slot for
/// it — so the already-accepted ability does not stop being a triggered mana
/// ability partway through its own resolution. CR 605.4a keeps the occurrence
/// stackless and owned by the immediate fixed point.
///
/// So: while the accepted-occurrence marker is live, the classification
/// decision has already been made and is simply read back. Outside such an
/// occurrence there is no marker and this delegates to the raw classifier with
/// the ambient `current_trigger_event`, which is exactly baseline. Ordinary
/// callers — including the compatibility `resolve_triggered_mana_ability_inline`
/// wrapper, which deliberately installs no marker — are unaffected.
pub(crate) fn is_resolving_triggered_mana(state: &GameState, ability: &ResolvedAbility) -> bool {
    if let Some(node) = state.active_accepted_triggered_mana_node {
        debug_assert_eq!(
            Some(node),
            state.active_rules_execution_node,
            "an accepted triggered-mana occurrence marker must name the ambient rules-execution \
             node; a mismatch means a scope was entered or restored without its partner"
        );
        return true;
    }
    is_triggered_mana_ability(ability, state.current_trigger_event.as_ref())
}

/// True iff every reachable link (via `sub_ability` and `else_ability` per
/// CR 608.2c) has `Effect::Mana`. The "every link is mana" rule is the
/// conservative reading of CR 605.1b(c) — inline resolution skips priority,
/// so any non-mana effect reachable during resolution forces stack use.
fn chain_is_all_mana(ability: &ResolvedAbility) -> bool {
    visit_links_all(ability, &|link| matches!(link.effect, Effect::Mana { .. }))
}

/// True iff **any** reachable link (via `sub_ability` and `else_ability`)
/// carries targets or a `multi_target` spec (CR 115.6 + CR 608.2c).
fn chain_has_any_targets(ability: &ResolvedAbility) -> bool {
    visit_links_any(ability, &|link| {
        !link.targets.is_empty() || link.multi_target.is_some()
    })
}

/// CR 105.3 + CR 106.1a: True iff any reachable link of this mana ability sets a
/// permanent's color to the mana produced earlier in the same activation — i.e.
/// carries a `ContinuousModification::AddChosenColor { .. }` ("… becomes that color",
/// Foraging Wickermaw). Gates the `ChosenAttribute::Color` record in
/// `produce_mana_from_ability` so ordinary producers (basics, City of Brass,
/// painlands, filter lands) never touch `chosen_attributes` — zero blast radius.
/// Built fresh per activation and walks only the activated ability's own chain.
fn chain_references_chosen_color(ability: &ResolvedAbility) -> bool {
    visit_links_any(ability, &|link| match &link.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|s| {
            s.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddChosenColor { .. }))
        }),
        _ => false,
    })
}

/// CR 106.1a: The single `ManaColor` this activation produced, or `None` if the
/// produced mana is empty, colorless, or spans more than one color. Reuses the
/// honest `mana_type_to_color` converter (Colorless → `None`). Used to bind
/// "that color" to the color of the mana the ability just made.
fn sole_produced_color(produced: &[ManaType]) -> Option<ManaColor> {
    let mut iter = produced.iter();
    let first = mana_type_to_color(*iter.next()?)?;
    for &mana_type in iter {
        if mana_type_to_color(mana_type)? != first {
            return None;
        }
    }
    Some(first)
}

/// Visit every reachable link of `ability` — head + `sub_ability` chain +
/// `else_ability` branches at each link — and return `true` iff `pred` holds
/// for all of them. Mirrors `chain_is_all_mana` / `chain_has_any_targets`'s
/// single traversal shape so the two walkers stay structurally identical.
fn visit_links_all(ability: &ResolvedAbility, pred: &dyn Fn(&ResolvedAbility) -> bool) -> bool {
    if !pred(ability) {
        return false;
    }
    if let Some(sub) = ability.sub_ability.as_deref() {
        if !visit_links_all(sub, pred) {
            return false;
        }
    }
    if let Some(else_branch) = ability.else_ability.as_deref() {
        if !visit_links_all(else_branch, pred) {
            return false;
        }
    }
    true
}

/// Dual of [`visit_links_all`]: returns `true` iff `pred` holds for any
/// reachable link.
fn visit_links_any(ability: &ResolvedAbility, pred: &dyn Fn(&ResolvedAbility) -> bool) -> bool {
    if pred(ability) {
        return true;
    }
    if let Some(sub) = ability.sub_ability.as_deref() {
        if visit_links_any(sub, pred) {
            return true;
        }
    }
    if let Some(else_branch) = ability.else_ability.as_deref() {
        if visit_links_any(else_branch, pred) {
            return true;
        }
    }
    false
}

/// CR 605.4a: Resolve a triggered mana ability inline (stack-skipped).
/// The ability's effect chain is executed immediately; mana additions land in the
/// controller's pool before any player could respond.
pub fn resolve_triggered_mana_ability_inline(
    state: &mut GameState,
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
) {
    // CR 603.3d: a triggered mana ability still resolves after its source has
    // left its zone. Production paths carry exact identity via the Plan-04
    // `trigger_source` context (captured at trigger time, LKI-safe); the
    // object lookup covers pre-P04 callers whose source is still present.
    // When neither is available (source gone AND no trigger context — only
    // synthetic/legacy callers), no dedicated journal node is begun: produced
    // mana falls back to the automatic Proposal attribution in
    // `add_mana_to_pool`, preserving pip conservation without fabricating an
    // exact incarnation identity.
    let source = ability
        .trigger_source
        .as_ref()
        .map(|context| context.identity.reference)
        .or_else(|| {
            state
                .objects
                .get(&ability.source_id)
                .map(crate::types::ObjectIncarnationRef::from_object)
        });
    let node = source.map(|source| {
        let caused_by = match trigger_event {
            Some(
                GameEvent::ManaAdded { source_id, .. }
                | GameEvent::TappedForMana { source_id, .. }
                | GameEvent::ManaAbilityProduced { source_id, .. },
            ) => state
                .resolved_rules_journal
                .latest_mana_producer_for_source(*source_id),
            _ => None,
        };
        state.begin_triggered_mana_journal_node(
            source,
            ability.trigger_definition_ref.clone(),
            caused_by,
        )
    });
    state.with_optional_rules_execution_node(node, |state| {
        let previous_trigger_event = state.current_trigger_event.clone();
        let previous_mana_override = state.current_triggered_mana_override.take();
        state.current_trigger_event = trigger_event.cloned();
        // Forward the planned color override so `effects::mana::resolve` can produce
        // the correct color for `AnyOneColor` triggered mana abilities (Fertile Ground)
        // rather than defaulting to `color_options.first()`.
        state.current_triggered_mana_override = color_override;
        // Use the standard resolution entry so sub_ability chains resolve uniformly.
        let _ = super::effects::resolve_ability_chain(state, ability, events, 0);
        state.current_triggered_mana_override = previous_mana_override;
        state.current_trigger_event = previous_trigger_event;
    });
}

/// CR 605.2: Mana abilities don't use the stack — they can't be targeted, countered, or responded to.
/// CR 605.3b: Mana abilities resolve immediately when activated.
///
/// Pays the full ability cost (tap, sacrifice, etc.) via `pay_mana_ability_cost`,
/// then produces mana. When `color_override` is `Some`, the choice dimension is
/// already resolved (auto-tap during cost payment): `SingleColor` replays a
/// single-color pick for `AnyOneColor`/`ChoiceAmongExiledColors`, while
/// `Combination` carries a full pre-chosen multi-mana sequence for
/// `ChoiceAmongCombinations` (filter lands).
pub fn resolve_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
) -> Result<(), EngineError> {
    // CR 605.3c: A top-level mana-ability activation has no suspended ancestor
    // on the call stack, so the in-flight exclusion chain starts empty. The
    // source itself is added downstream in `pay_mana_sub_cost`.
    resolve_mana_ability_excluding(
        state,
        source_id,
        player,
        ability_def,
        events,
        color_override,
        &HashSet::new(),
        None,
        None,
        None,
    )
}

/// Resolve a mana ability while excluding an in-flight chain of ancestor
/// mana-ability sources from the cost-payment auto-tap (CR 605.3c). Called by
/// the casting auto-tap (`auto_tap_mana_sources_inner`) when paying one mana
/// ability's mana sub-cost forces activation of further mana abilities: each
/// ancestor activation is synchronously suspended mid-payment on the Rust call
/// stack and must not be re-activated, or the auto-tap recurses infinitely
/// (two cross-paying Signets, an N-source chain, or a self-loop).
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_mana_ability_excluding(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
    excluded_sources: &HashSet<ObjectId>,
    // CR 107.4b + CR 118.10: When this ability is being activated to fund an
    // outer cost (nested Phase-3 auto-tap), the outer cost's colored shard demand
    // is threaded here so this ability's own mana sub-cost is funded from
    // non-demanded mana, never a floated color the outer cost still needs. `None`
    // at the top-level entry — there is no outer cost on the stack.
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    resume: Option<&ManaAbilityResume>,
    parent: Option<&ManaAbilityCostParent>,
) -> Result<(), EngineError> {
    let waiting_before = state.waiting_for.clone();
    let ability_index = state.objects.get(&source_id).and_then(|object| {
        object
            .abilities
            .iter()
            .position(|ability| ability == ability_def)
    });
    let rules_execution_node = Some(state.begin_activated_mana_journal_node(source_id));
    let pending = PendingManaAbility {
        player,
        source_id,
        ability_index,
        rules_execution_node,
        ability_snapshot: Some(ability_def.clone()),
        color_override,
        // The direct resolver normally leaves its caller's waiting root
        // untouched.  Its ordinary completion must therefore return to
        // priority; the outer typed payment root lives exclusively in
        // `cost_move_resume` and is promoted only when a replaceable cost
        // move actually pauses.  Reusing that root here would make a
        // synchronous auto-tap recursively retry its still-live caller.
        resume: ManaAbilityResume::Priority,
        cost_move_resume: resume.cloned(),
        chosen_tappers: Vec::new(),
        chosen_discards: Vec::new(),
        chosen_mana_payment: None,
        chosen_counter_count: None,
        chosen_x: None,
        collected_evidence: Vec::new(),
        chosen_exiled: Vec::new(),
        chosen_sacrificed_battlefield: Vec::new(),
        cost_paid_object: None,
        batch_siblings: Vec::new(),
    };
    // CR 605.3b + CR 616.1: This direct path is used by auto-tap and retains
    // its historical default-output semantics even when a cost move pauses.
    // The cursor serializes that resolution mode with the exact outer resume.
    let cost_event_start = events.len();
    // CR 603.2 + CR 603.3b: Prepare a fresh child-facing snapshot of the live
    // synchronous parent carrying that parent frame's current unscanned suffix,
    // including every earlier sibling that already completed synchronously. The
    // live parent cursor is never mutated, so a synchronous child drops this
    // snapshot without duplicating the root's eventual scan.
    let prepared_parent =
        parent.map(|parent| parent_snapshot_with_current_cost_events(parent, events));
    let waiting_for = continue_mana_ability_cost_payment(
        state,
        pending,
        mana_ability_cost_cursor(
            &ability_def.cost,
            excluded_sources,
            sub_cost_demand,
            ManaAbilityCostResolutionMode::AutoResolved,
            prepared_parent.as_ref(),
        ),
        events,
        cost_event_start,
    )?;
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        state.waiting_for = waiting_for;
    } else {
        state.waiting_for = waiting_before;
    }
    Ok(())
}

/// Produce mana from a resolved mana ability without paying costs.
/// Shared by `resolve_mana_ability` (cost paid inline) and `handle_choose_mana_color`
/// (cost already paid during the `TapCreaturesForManaAbility` phase).
///
/// `cost_paid_object` carries the captured public characteristics of any
/// object exiled or sacrificed as part of cost payment so production counts can
/// resolve cost-paid-object refs (Food Chain / Burnt Offering class).
#[allow(clippy::too_many_arguments)]
fn produce_mana_from_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
    chosen_x: Option<u32>,
    cost_paid_object: Option<CostPaidObjectSnapshot>,
) {
    // CR 117.1 + CR 202.3: Build a transient `ResolvedAbility` carrying the
    // cost-paid object snapshot so quantity resolution sees it. Reused for
    // both production-count and sub-chain resolution paths so the same
    // snapshot is visible end-to-end.
    let resolved_for_quantity = resolved_mana_ability_for_current_state(
        state,
        source_id,
        player,
        ability_def,
        chosen_x,
        cost_paid_object,
    );

    // CR 106.12: a permanent is "tapped for mana" when the activated mana
    // ability's cost includes the `{T}` symbol.
    let tapped = mana_sources::has_tap_component(&ability_def.cost);

    // CR 101.4 + CR 608.2c + CR 605.3b: A mana ability may instruct each player to add
    // mana (Yurlok). It is still one mana ability resolving immediately, but
    // the production instruction is performed once for every matching player
    // in APNAP order. Preserve the printed controller separately while
    // rebinding the acting/scoped player, exactly like the general
    // `player_scope` resolution driver.
    let recipients = ability_def.player_scope.as_ref().map_or_else(
        || vec![player],
        |scope| {
            crate::game::players::apnap_order(state)
                .into_iter()
                .filter(|recipient| {
                    super::effects::matches_player_scope(
                        state, *recipient, scope, player, source_id,
                    )
                })
                .collect()
        },
    );
    let mut produced_for_tap_event = Vec::new();
    let mut produced_for_ability_events = Vec::new();
    for recipient in recipients {
        let mut scoped = resolved_for_quantity.clone();
        scoped.set_original_controller_recursive(player);
        scoped.set_controller_recursive(recipient);
        scoped.set_scoped_player_recursive(recipient);

        // CR 106.6: Resolve spend-restriction templates, grants, and expiry so
        // they attach to each produced `ManaUnit`.
        let (produced_mana, restrictions, grants, expiry, source_could_produce_two_or_more_colors) =
            match &scoped.effect {
                Effect::Mana {
                    produced,
                    restrictions,
                    grants,
                    expiry,
                    target: None,
                } => {
                    let mana = match color_override.clone() {
                        // `Combination` is pre-chosen — skip `resolve_mana_types`
                        // so the exact sequence lands in the pool (CR 605.3b).
                        Some(ProductionOverride::Combination(types)) => types,
                        Some(ProductionOverride::SingleColor(color)) => {
                            resolve_single_color_override(state, produced, &scoped, color)
                        }
                        None => super::effects::mana::resolve_mana_types_for_ability(
                            produced, state, &scoped,
                        ),
                    };
                    let concrete = resolve_restrictions(restrictions, state, source_id);
                    let source_could_produce_two_or_more_colors =
                        mana_sources::source_could_produce_two_or_more_colors(
                            state, source_id, player,
                        );
                    (
                        mana,
                        concrete,
                        grants.clone(),
                        *expiry,
                        source_could_produce_two_or_more_colors,
                    )
                }
                _ => (Vec::new(), Vec::new(), Vec::new(), None, false),
            };

        // CR 106.12a: `TappedForMana` is one source-level event for this
        // resolution. Its payload is the full aggregate produced by the
        // ability, including scoped recipients that exclude the activator.
        produced_for_tap_event.extend(produced_mana.iter().copied());
        if !produced_mana.is_empty() {
            produced_for_ability_events.push((recipient, produced_mana.clone()));
        }
        for &mana_type in &produced_mana {
            mana_payment::produce_mana_with_attributes_from_source_quality(
                state,
                source_id,
                mana_type,
                recipient,
                tapped,
                source_could_produce_two_or_more_colors,
                &restrictions,
                &grants,
                expiry,
                events,
            );
        }
    }

    // CR 105.3 + CR 106.1a + CR 605.3b: If a later clause in THIS mana ability
    // sets a permanent's color to the mana just produced ("… becomes that
    // color", Foraging Wickermaw), record the produced color on the source so
    // the downstream `AddChosenColor` (Layer 5, CR 613.1e) reads it live. Gated
    // on the chain actually carrying an `AddChosenColor`, so ordinary producers
    // are untouched. Placed above the `TappedForMana` push below, which MOVES
    // `produced_mana`.
    if chain_references_chosen_color(&resolved_for_quantity) {
        if let Some(color) = sole_produced_color(&produced_for_tap_event) {
            if let Some(obj) = state.objects.get_mut(&source_id) {
                // CR 400.7: `chosen_attributes` persist on the permanent until it
                // changes zones, and `chosen_color()` returns the FIRST match, so
                // a re-activation on a later turn must OVERWRITE — retain-drop any
                // prior `Color`, then push the current one (not accumulate).
                obj.chosen_attributes
                    .retain(|a| !matches!(a, ChosenAttribute::Color(_)));
                obj.chosen_attributes.push(ChosenAttribute::Color(color));
            }
        }
    }

    // CR 605.1b: Emit one aggregate event per receiving player for every
    // mana-ability resolution, including abilities without a tap cost. Its
    // output vector lets triggered mana abilities inspect each player's share
    // of a multi-recipient resolution exactly once.
    for (recipient, produced) in produced_for_ability_events {
        events.push(GameEvent::ManaAbilityProduced {
            player_id: recipient,
            source_id,
            produced,
            trigger_state: crate::types::events::ManaAbilityTriggerState::Pending,
        });
    }

    // CR 106.12a: an "is tapped for mana" trigger fires once per resolution of
    // a `{T}`-cost mana ability that produces mana — not once per mana unit.
    // Emit a single `TappedForMana` here so the `TapsForMana` matcher fires
    // exactly once (the per-unit `ManaAdded` events above remain pool
    // accounting only).
    if tapped && !produced_for_tap_event.is_empty() {
        events.push(GameEvent::TappedForMana {
            player_id: player,
            source_id,
            produced: produced_for_tap_event,
            tap_state: ManaTapState::from_tap(tapped),
        });
    }

    // CR 605.3b + CR 605.1a: A mana ability with a non-mana clause in its
    // effect chain (e.g. painlands' "This land deals 1 damage to you.")
    // resolves that chain inline — mana abilities don't use the stack, so
    // the sub-ability runs as part of the same atomic resolution.
    resolve_mana_ability_sub_chain(state, &resolved_for_quantity, events);
}

fn resolved_mana_ability_for_current_state(
    state: &GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    chosen_x: Option<u32>,
    cost_paid_object: Option<CostPaidObjectSnapshot>,
) -> ResolvedAbility {
    let mut resolved =
        super::ability_utils::build_resolved_from_def(ability_def, source_id, player);
    if let Some(snapshot) = cost_paid_object {
        resolved.set_cost_paid_object_recursive(snapshot);
    }
    // CR 107.3a/.3c: bind the announced X into the resolved ability so produced
    // mana counts (`AnyCombination { count: Ref(Variable "X") }`) and any X-bearing
    // sub-effects resolve to the chosen value (Chicago Loop's `Add X mana`).
    if let Some(x) = chosen_x {
        resolved.set_chosen_x_recursive(x);
    }
    apply_condition_instead_mana_swap(state, &resolved)
}

pub(crate) fn apply_condition_instead_mana_swap(
    state: &GameState,
    ability: &ResolvedAbility,
) -> ResolvedAbility {
    let Some(sub) = ability.sub_ability.as_deref() else {
        return ability.clone();
    };
    let Some(AbilityCondition::ConditionInstead { inner }) = sub.condition.as_ref() else {
        return ability.clone();
    };
    if super::effects::evaluate_condition(inner, state, ability) {
        if matches!(sub.effect, Effect::Mana { target: None, .. }) {
            return super::ability_utils::apply_instead_swap(ability, sub);
        }
        return ability.clone();
    }

    let mut base = ability.clone();
    base.sub_ability = sub.else_ability.clone();
    base
}

fn resolve_single_color_override(
    state: &mut GameState,
    produced: &ManaProduction,
    ability: &ResolvedAbility,
    color: ManaType,
) -> Vec<ManaType> {
    let previous_choice = if matches!(produced, ManaProduction::ChosenColor { .. }) {
        let Some(chosen_color) = mana_type_to_color(color) else {
            return Vec::new();
        };
        let previous = state.last_named_choice.take();
        state.last_named_choice = Some(ChoiceValue::Color(chosen_color));
        Some(previous)
    } else {
        None
    };

    let resolved = super::effects::mana::resolve_mana_types_for_ability(produced, state, ability);

    if let Some(previous) = previous_choice {
        state.last_named_choice = previous;
    }

    vec![color; resolved.len()]
}

/// CR 605.3b: Mana abilities resolve immediately unless paying the cost requires a choice.
#[allow(clippy::too_many_arguments)]
pub fn activate_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    resume: ManaAbilityResume,
    color_override: Option<ProductionOverride>,
) -> Result<WaitingFor, EngineError> {
    let source = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Mana ability source not found".to_string()))?;
    // CR 702.26b: Phased-out permanents are treated as though they do not
    // exist, so they cannot activate abilities.
    if source.is_phased_out_permanent() {
        return Err(EngineError::ActionNotAllowed(
            "Phased-out permanents cannot activate abilities (CR 702.26b)".to_string(),
        ));
    }
    if source.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if source.zone != required_zone {
        return Err(EngineError::InvalidAction(format!(
            "Object is not in the correct zone (expected {:?})",
            required_zone
        )));
    }
    // CR 602.5: enforce activation prohibitions at the executor, not just at
    // legal-action filtering — a buggy or hostile client may submit
    // `GameAction::ActivateAbility` directly. The mana-ability fast path must
    // honor the same static-ability gates that `casting::handle_activate_ability`
    // applies on the non-mana path, so City of Solitude (CantActivateDuring with
    // exemption: None) and any future CantBeActivated with exemption: None block
    // mana activations as the rules require.
    if super::casting::is_blocked_by_cant_be_activated(state, player, source_id, ability_def) {
        return Err(EngineError::ActionNotAllowed(
            "Activated abilities of this permanent can't be activated (CR 602.5)".to_string(),
        ));
    }
    if super::casting::is_blocked_by_cant_activate_during(state, player, ability_def) {
        return Err(EngineError::ActionNotAllowed(
            "Activated abilities can't be activated during this turn (CR 602.5 + CR 117.1b)"
                .to_string(),
        ));
    }
    super::restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    let rules_execution_node = Some(state.begin_activated_mana_journal_node(source_id));
    advance_mana_ability_activation(
        state,
        PendingManaAbility {
            player,
            source_id,
            ability_index: Some(ability_index),
            rules_execution_node,
            ability_snapshot: Some(ability_def.clone()),
            color_override,
            resume,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        },
        events,
    )
}

fn complete_mana_ability_activation(
    state: &mut GameState,
    source_id: ObjectId,
    ability_index: Option<usize>,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let Some(ability_index) = ability_index else {
        return;
    };
    super::restrictions::record_ability_activation(state, source_id, ability_index);
    super::casting_targets::emit_keyword_ability_event_if_tagged(
        state,
        source_id,
        ability_index,
        player,
        events,
    );
}

/// Extract the prompt shape for a mana ability that requires a player choice.
///
/// Returns `Some(ManaChoicePrompt::SingleColor)` when the player must pick one
/// color from a set (AnyOneColor, ChoiceAmongExiledColors) and
/// `Some(ManaChoicePrompt::Combination)` when the player must pick one of
/// several fixed multi-mana sequences (filter lands). Returns
/// `Some(ManaChoicePrompt::AnyCombination)` when each produced mana unit has
/// an independent color choice. Returns `None` when production is fully
/// determined (Fixed, Colorless, single-option AnyOneColor).
/// `color_ability` retains the original target context for dynamic color
/// discovery; `count_ability` may be scoped to the count-source target for
/// quantity resolution.
pub(crate) fn mana_choice_prompt(
    effect: &Effect,
    state: &GameState,
    source_id: ObjectId,
    color_ability: Option<&ResolvedAbility>,
    count_ability: Option<&ResolvedAbility>,
) -> Option<ManaChoicePrompt> {
    let Effect::Mana { produced, .. } = effect else {
        return None;
    };
    match produced {
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() > 1 => {
            // CR 106.5: An ability that would produce mana of an undefined type
            // produces no mana, so it needs no color choice.
            let produces_mana = count_ability
                .map(|ability| {
                    !super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                        .is_empty()
                })
                .unwrap_or(true);
            produces_mana.then(|| ManaChoicePrompt::SingleColor {
                options: color_options.iter().map(mana_color_to_type).collect(),
            })
        }
        ManaProduction::AnyCombination { color_options, .. } if color_options.len() > 1 => {
            let ability = count_ability?;
            let count =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                    .len();
            if count > 0 {
                Some(ManaChoicePrompt::AnyCombination {
                    count,
                    options: color_options.iter().map(mana_color_to_type).collect(),
                })
            } else {
                None
            }
        }
        ManaProduction::ChoiceAmongExiledColors { source } => {
            let options = super::effects::mana::exiled_color_options(state, *source, source_id);
            if options.len() > 1 {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        ManaProduction::AnyOneColorAmongPermanents { filter, .. } => {
            // CR 106.1: Player chooses one of the colors among matching permanents they
            // control.
            let options = super::effects::mana::distinct_colors_among_permanents(
                state,
                color_ability,
                source_id,
                filter,
            )
            .into_iter()
            .map(|color| mana_color_to_type(&color))
            .collect::<Vec<_>>();
            // CR 106.5: An ability that would produce mana of an undefined type
            // produces no mana, so it needs no color choice.
            let produces_mana = count_ability
                .map(|ability| {
                    !super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                        .is_empty()
                })
                .unwrap_or(true);
            if options.len() > 1 && produces_mana {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        // CR 605.3b: Filter lands — pick one of N fixed multi-mana combinations.
        ManaProduction::ChoiceAmongCombinations { options } if options.len() > 1 => {
            Some(ManaChoicePrompt::Combination {
                options: options
                    .iter()
                    .map(|combo| combo.iter().map(mana_color_to_type).collect())
                    .collect(),
            })
        }
        ManaProduction::ChosenColor {
            fixed_alternative, ..
        } => {
            if fixed_alternative.is_some() {
                // CR 106.5: A fixed alternative makes production resolvable
                // before the color choice. If it resolves to no mana, no color
                // choice is needed. Pure chosen-color production must retain
                // its prompt because that choice can determine its count.
                let produces_mana = count_ability
                    .map(|ability| {
                        !super::effects::mana::resolve_mana_types_for_ability(
                            produced, state, ability,
                        )
                        .is_empty()
                    })
                    .unwrap_or(true);
                if !produces_mana {
                    return None;
                }
            }
            let chosen = super::effects::mana::chosen_color_for_mana(state, source_id);
            match (fixed_alternative, chosen) {
                // CR 106.1: "Add {fixed} or one mana of the chosen color" — once
                // a color is chosen, the player still picks between the fixed
                // color and the chosen color. Dedupe defensively: identical
                // options collapse to a 1-element set (no prompt).
                (Some(fixed), Some(chosen)) => {
                    let mut options = vec![mana_color_to_type(fixed)];
                    let chosen_type = mana_color_to_type(&chosen);
                    if !options.contains(&chosen_type) {
                        options.push(chosen_type);
                    }
                    if options.len() > 1 {
                        Some(ManaChoicePrompt::SingleColor { options })
                    } else {
                        None
                    }
                }
                // CR 106.1: no color chosen yet (cannot occur for Gate lands —
                // the as-enters Choose always fires first — but the field makes
                // it representable). The fixed color is a subset of ALL, so a
                // full five-color prompt loses nothing.
                (Some(_), None) | (None, None) => Some(ManaChoicePrompt::SingleColor {
                    options: ManaColor::ALL.iter().map(mana_color_to_type).collect(),
                }),
                // CR 106.1: pure chosen-color production with a color already
                // chosen — no prompt (Utopia Sprawl class).
                (None, Some(_)) => None,
            }
        }
        // CR 106.7 + CR 106.1b: Reflecting Pool class — surface the union of
        // mana types that filter-matching lands could produce, including
        // `Colorless`. With 0 or 1 options the resolver handles it without a
        // prompt (CR 106.5: empty union → no mana; single option auto-picks).
        ManaProduction::AnyTypeProduceableBy { land_filter, .. } => {
            let owner = state.objects.get(&source_id).map(|obj| obj.controller)?;
            let options = super::mana_sources::produceable_mana_types_by_filter(
                state,
                land_filter,
                owner,
                source_id,
            );
            // CR 106.5: An ability that would produce mana of an undefined type
            // produces no mana, so it needs no color choice.
            let produces_mana = count_ability
                .map(|ability| {
                    !super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                        .is_empty()
                })
                .unwrap_or(true);
            if options.len() > 1 && produces_mana {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        // CR 903.4 + CR 903.4f + CR 106.5: Dynamically resolve the activator's
        // commander color identity. If the identity contains 0 or 1 colors,
        // the resolver handles it without a prompt (CR 106.5: undefined color
        // produces no mana; single-color identity auto-picks).
        ManaProduction::AnyInCommandersColorIdentity { .. } => {
            let owner = state.objects.get(&source_id).map(|obj| obj.controller)?;
            let identity = super::commander::commander_color_identity(state, owner);
            // CR 106.5: An ability that would produce mana of an undefined type
            // produces no mana, so it needs no color choice.
            let produces_mana = count_ability
                .map(|ability| {
                    !super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                        .is_empty()
                })
                .unwrap_or(true);
            if identity.len() > 1 && produces_mana {
                Some(ManaChoicePrompt::SingleColor {
                    options: identity.iter().map(mana_color_to_type).collect(),
                })
            } else {
                None
            }
        }
        // CR 106.7 (issue #1556): Exotic Orchard class — "add one mana of any
        // color that a land an opponent controls could produce." Surface the
        // union of producible colors as a choice; with 0 or 1 option the
        // resolver handles it without a prompt (CR 106.5: empty union → no mana;
        // single option auto-picks). Mirrors `AnyTypeProduceableBy`.
        ManaProduction::OpponentLandColors { .. } => {
            let owner = state.objects.get(&source_id).map(|obj| obj.controller)?;
            let options = super::mana_sources::opponent_land_color_options(state, owner);
            // CR 106.5: An ability that would produce mana of an undefined type
            // produces no mana, so it needs no color choice.
            let produces_mana = count_ability
                .map(|ability| {
                    !super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                        .is_empty()
                })
                .unwrap_or(true);
            if options.len() > 1 && produces_mana {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        // CR 106.1 + CR 202.2c: Omnath, Locus of All — each of the produced mana
        // is freely chosen among the scoped object's colors (dynamic, mirrors
        // AnyCombination but with a runtime-resolved option set). Surface the
        // AnyCombination prompt only when the object has more than one color; 0 or
        // 1 color needs no prompt (CR 106.5 empty → no mana; single auto-picks).
        ManaProduction::AnyCombinationOfObjectColors { scope, .. } => {
            let options =
                super::effects::mana::object_colors_for_scope(state, color_ability, *scope)
                    .iter()
                    .map(mana_color_to_type)
                    .collect::<Vec<_>>();
            if options.len() <= 1 {
                return None;
            }
            let ability = count_ability?;
            let count =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                    .len();
            if count > 0 {
                Some(ManaChoicePrompt::AnyCombination { count, options })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// CR 605.3b: Complete the mana color/combination choice. Cost was already
/// paid before the prompt (either in `activate_mana_ability` or
/// `handle_tap_creatures_for_mana_ability`), so this only produces mana.
/// The `choice` shape must match the `prompt` shape — the engine rejects
/// mismatches (e.g., answering `Combination` to a `SingleColor` prompt).
pub fn handle_choose_mana_color(
    state: &mut GameState,
    pending: &PendingManaAbility,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let override_value = match (prompt, chosen) {
        (ManaChoicePrompt::SingleColor { options }, ManaChoice::SingleColor(color)) => {
            if !options.contains(&color) {
                return Err(EngineError::InvalidAction(
                    "Chosen color is not among the legal options".to_string(),
                ));
            }
            ProductionOverride::SingleColor(color)
        }
        (ManaChoicePrompt::Combination { options }, ManaChoice::Combination(combo)) => {
            if !options.iter().any(|opt| opt == &combo) {
                return Err(EngineError::InvalidAction(
                    "Chosen combination is not among the legal options".to_string(),
                ));
            }
            ProductionOverride::Combination(combo)
        }
        (ManaChoicePrompt::AnyCombination { count, options }, ManaChoice::Combination(combo)) => {
            if combo.len() != *count || combo.iter().any(|color| !options.contains(color)) {
                return Err(EngineError::InvalidAction(
                    "Chosen mana combination is not legal for this prompt".to_string(),
                ));
            }
            ProductionOverride::Combination(combo)
        }
        _ => {
            return Err(EngineError::InvalidAction(
                "Mana choice shape does not match the active prompt".to_string(),
            ));
        }
    };

    let ability_def = state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| {
            pending
                .ability_index
                .and_then(|index| obj.abilities.get(index))
        })
        .cloned()
        .or_else(|| pending.ability_snapshot.clone())
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))?;

    let node = pending
        .rules_execution_node
        .unwrap_or_else(|| state.begin_activated_mana_journal_node(pending.source_id));
    // CR 605.4a: the choice action's own live-collection start. The already-paid
    // cost range was collected by the frame that returned this prompt; this
    // action owns only what production/completion emits from here.
    let choice_action_start = events.len();
    state.with_rules_execution_node(node, |state| {
        produce_mana_from_ability(
            state,
            pending.source_id,
            pending.player,
            &ability_def,
            events,
            Some(override_value),
            pending.chosen_x,
            pending.cost_paid_object.clone(),
        );
        complete_mana_ability_activation(
            state,
            pending.source_id,
            pending.ability_index,
            pending.player,
            events,
        );
    });

    // CR 603.2 + CR 605.3c + CR 605.4a: the second typed half of the colour
    // seam. The frame this choice completes is a completed mana frame with an
    // empty durable ledger, so it runs the SAME collection helper before
    // returning its resume owner — and before `batch_activate_mana_siblings`
    // begins the next sibling, each of which performs the same helper on its own
    // finish path. That is the CR 605.4a pre-pass between siblings, in place of
    // one aggregate scan after the loop.
    if let Some(pause) = collect_completed_mana_frame_events(
        state,
        Vec::new(),
        events,
        choice_action_start,
        ManaTriggerFixedPointResume::Root {
            player: pending.player,
            resume: Box::new(pending.resume.clone()),
        },
    ) {
        return Ok(pause);
    }

    Ok(resume_waiting_for(pending.player, pending.resume.clone()))
}

/// CR 605.3a: Bulk-activate the controller's other identical, choice-free mana
/// sources (their remaining Treasures, etc.) with the color just chosen for a
/// `SingleColor` prompt. Runs immediately after `handle_choose_mana_color` has
/// resolved the originally-tapped source; together they activate `count` sources
/// in one `ChooseManaColor` round-trip.
///
/// Each sibling is an independent activated mana ability that resolves
/// immediately and before the next is begun (CR 605.3c), without using the stack
/// (CR 605.3b) — so no player gains priority between them. Cost-payment and mana
/// events append to `events`; the caller's single post-handler trigger scan then
/// fires each sacrifice's observers (Mayhem Devil, Korvold, Cruel Celebrant, …)
/// exactly once. `pending.batch_siblings` was pre-filtered to choice-free,
/// currently-activatable twins (see `cost_resolves_without_choice` /
/// `batch_eligible_siblings`), so no sibling can surface a further interactive
/// prompt — that invariant is asserted below rather than handled.
pub(crate) fn batch_activate_mana_siblings(
    state: &mut GameState,
    pending: &PendingManaAbility,
    chosen: &ManaChoice,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let ManaChoice::SingleColor(color) = chosen else {
        return Err(EngineError::InvalidAction(
            "Bulk mana activation is only valid for a single-color choice".to_string(),
        ));
    };
    // `count` is validated against `batch_siblings.len() + 1` by the dispatcher
    // before any mana is produced, so `extra` never exceeds the sibling list and
    // `take` is exact.
    let extra = (count as usize).saturating_sub(1);

    // The originally-activated source's mana ability is the shape every sibling
    // was selected to match. Re-resolve each sibling's matching ability index
    // (a sibling may carry unrelated abilities too).
    let reference_def = mana_ability_definition(state, pending)?;

    for &sibling_id in pending.batch_siblings.iter().take(extra) {
        let Some((index, def)) = state.objects.get(&sibling_id).and_then(|obj| {
            obj.abilities
                .iter()
                .position(|ability| *ability == reference_def)
                .map(|index| (index, obj.abilities[index].clone()))
        }) else {
            return Err(EngineError::InvalidAction(
                "Bulk mana source is no longer available".to_string(),
            ));
        };
        // CR 605.3a + CR 605.3b: independent mana ability, no stack, color fixed.
        let resume = activate_mana_ability(
            state,
            sibling_id,
            pending.player,
            index,
            &def,
            events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(*color)),
        )?;
        debug_assert!(
            matches!(resume, WaitingFor::Priority { .. }),
            "batched choice-free mana sibling returned an interactive state: {resume:?}"
        );
    }
    Ok(())
}

/// CR 118.3 / CR 605.3b: Complete the tapped-creature choice, then resolve the mana ability.
pub fn handle_tap_creatures_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_creatures: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must tap exactly {} creature(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for mana ability cost".to_string(),
            ));
        }
    }

    let mut updated = pending.clone();
    updated.chosen_tappers = chosen.to_vec();
    advance_mana_ability_activation(state, updated, events)
}

/// CR 117.1 + CR 118.3 + CR 605.3b + CR 400.7j: Complete a non-self exile
/// mana-ability cost selection. Captures the cost-paid object's public
/// characteristics before the cost is paid, then resumes the activation flow.
pub fn handle_exile_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_cards: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must exile exactly {} card(s), got {}",
            count,
            chosen.len()
        )));
    }
    if contains_duplicate_object_id(chosen) {
        return Err(EngineError::InvalidAction(
            "Cannot exile the same card more than once for a mana ability cost".to_string(),
        ));
    }
    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card not eligible for mana ability exile cost".to_string(),
            ));
        }
        // CR 118.10: One payment cannot apply to both this mana ability cost
        // and a pending spell sacrifice cost.
        if deferred_spell_sacrifice_reserved(state, *id) {
            return Err(EngineError::InvalidAction(
                "Selected card is already committed to a spell sacrifice cost".to_string(),
            ));
        }
    }

    // CR 117.1 + CR 400.7j + CR 608.2k: Capture the cost-paid object's public
    // characteristics before it leaves its zone.
    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });

    let mut updated = pending.clone();
    updated.chosen_exiled = chosen.to_vec();
    updated.cost_paid_object = captured;
    advance_mana_ability_activation(state, updated, events)
}

/// CR 117.1 + CR 118.3 + CR 605.3b + CR 202.3: Complete the
/// sacrifice-from-battlefield mana-ability cost selection (Phyrexian Altar class).
/// Captures the cost-paid object's public characteristics before sacrifice so
/// mana production can reference the sacrificed object's mana value when needed.
pub fn handle_sacrifice_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_permanents: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must sacrifice exactly {} permanent(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible for mana ability sacrifice cost".to_string(),
            ));
        }
        // CR 118.10: One payment cannot apply to both this mana ability cost
        // and a pending spell sacrifice cost.
        if deferred_spell_sacrifice_reserved(state, *id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent is already committed to a spell sacrifice cost".to_string(),
            ));
        }
    }

    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });

    let mut updated = pending.clone();
    updated.chosen_sacrificed_battlefield = chosen.to_vec();
    updated.cost_paid_object = captured;
    advance_mana_ability_activation(state, updated, events)
}

fn deferred_spell_sacrifice_reserved(state: &GameState, object_id: ObjectId) -> bool {
    state.pending_cast.as_ref().is_some_and(|pending| {
        pending
            .deferred_sacrificed_permanents
            .iter()
            .any(|selection| selection.object_id == object_id)
    })
}

pub fn handle_discard_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_cards: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must discard exactly {} card(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card not eligible for mana ability cost".to_string(),
            ));
        }
    }

    let mut updated = pending.clone();
    updated.chosen_discards = chosen.to_vec();
    advance_mana_ability_activation(state, updated, events)
}

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static MANA_READINESS_CALLS: AtomicUsize = AtomicUsize::new(0);

/// CR 602.5 + CR 605.3a: True iff this mana ability is activatable right now
/// using only non-simulating, recursion-free gates — the simulation-free prefix
/// of `activate_mana_ability`. Single authority shared by the
/// `can_activate_mana_ability_now` pre-clone gate and the `batch_eligible_siblings`
/// sibling filter, so both agree on readiness without each cloning + recursing
/// the whole game state (the O(N!) cause when N batchable sources are present).
/// CR 602.5 + CR 604.1: hoistable existence gates for the two whole-battlefield
/// activation-prohibition scans inside [`mana_ability_ready_without_simulation`].
///
/// `is_blocked_by_cant_be_activated` (CR 602.5, City of Solitude class) and
/// `is_blocked_by_cant_activate_during` (CR 117.1b) each iterate every
/// battlefield static. Calling them per mana source turns the board-global mana
/// availability sweep into O(N^2) (~700 Cryptolith-Rite tokens × ~700 statics).
/// Computing presence ONCE and gating each scan collapses the sweep to O(N) when
/// no such static exists (the overwhelming common case). Mirrors
/// `combat::CombatStaticGates`. Uses `game_functioning_statics` (a superset of
/// the precise `battlefield_active_statics` the scans use) so a `false` gate is a
/// sound skip; a `true` gate falls through to the exact per-source scan.
#[derive(Debug, Clone, Copy)]
pub struct ManaActivationGates {
    has_cant_be_activated: bool,
    has_cant_activate_during: bool,
}

impl ManaActivationGates {
    /// Reads both presence flags from the O(1) `StaticModePresence` index (Unit 1)
    /// instead of sweeping `game_functioning_statics`. A post-flush-precise superset:
    /// a spurious `true` falls through to the exact per-source scan.
    pub fn compute(state: &GameState) -> Self {
        ManaActivationGates {
            has_cant_be_activated: static_kind_present(state, StaticModeKind::CantBeActivated),
            has_cant_activate_during: static_kind_present(
                state,
                StaticModeKind::CantActivateDuring,
            ),
        }
    }
}

/// CR 305.6: builds the minimal synthetic `AbilityDefinition` (Tap cost,
/// `Effect::Mana`) standing in for a land's INTRINSIC "{T}: Add [mana
/// symbol]" ability — the ability every land with a basic land type has
/// whether or not any `AbilityDefinition` object represents it. Used so a
/// legality check's `kind`/`exemption`/cost axes (e.g. Damping Matrix's
/// "unless they're mana abilities" carve-out) evaluate identically to how
/// they would against a real, printed mana ability.
fn intrinsic_land_mana_ability_definition(color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        crate::types::ability::AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: crate::types::ability::ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap)
}

/// CR 305.6 + CR 602.5: Is a land's basic-land-type INTRINSIC mana ability
/// currently blocked — by ANY of the gates a printed mana ability would be
/// checked against? `mana_sources::land_mana_options`'s bare-subtype
/// fallback synthesizes a `ManaSourceOption` for a land with no
/// `AbilityDefinition` object at all (Urborg/Blood-Moon-class grants), but
/// CR 305.6's intrinsic ability is still an activated mana ability, so every
/// gate `mana_ability_ready_without_simulation_gated` applies to a real one —
/// phased-out (CR 702.26b), detained (CR 701.35a), zone (CR 113.6), tapped/
/// can't-tap (CR 101.2 + CR 107.5 + CR 601.2h + CR 602.2b), summoning sickness
/// (CR 302.6), CantBeActivated/CantActivateDuring (CR 602.5), static
/// activation restrictions (CR 604/605.3b) — must apply to it too. Routes the
/// synthetic definition through that SAME single-authority readiness check
/// rather than re-implementing any subset of it: the function takes an
/// `AbilityDefinition` by reference and never indexes `obj.abilities`, so a
/// synthesized definition with no real storage slot is exactly as valid an
/// input as a printed one. `ability_index: 0` is inert here — the intrinsic
/// ability carries empty `activation_restrictions` (so `ability_index` is
/// never read by that check) and a bare `Tap` cost (whose payability check
/// doesn't consult it either).
pub(crate) fn intrinsic_land_mana_ability_blocked(
    state: &GameState,
    controller: PlayerId,
    object_id: ObjectId,
    color: ManaColor,
    gates: Option<&ManaActivationGates>,
) -> bool {
    let ability_def = intrinsic_land_mana_ability_definition(color);
    let ready = match gates {
        Some(gates) => mana_ability_ready_without_simulation_gated(
            state,
            controller,
            object_id,
            0,
            &ability_def,
            gates,
        ),
        None => {
            mana_ability_ready_without_simulation(state, controller, object_id, 0, &ability_def)
        }
    };
    !ready
}

fn mana_ability_ready_without_simulation(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    // Single-call entry: compute the gates once (one battlefield scan) and
    // delegate. The board-sweep caller (`derive_display_state`) hoists the gates
    // across all sources via `..._gated` instead.
    let gates = ManaActivationGates::compute(state);
    mana_ability_ready_without_simulation_gated(
        state,
        player,
        source_id,
        ability_index,
        ability_def,
        &gates,
    )
}

fn mana_ability_ready_without_simulation_gated(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    gates: &ManaActivationGates,
) -> bool {
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };
    // CR 702.26b: Phased-out permanents are treated as though they do not
    // exist, so they cannot activate abilities.
    if obj.is_phased_out_permanent() {
        return false;
    }
    // CR 701.35a: Detained permanents' activated abilities can't be activated.
    if !obj.detained_by.is_empty() {
        return false;
    }
    // CR 602.2a: Only the controller may activate the ability.
    if obj.controller != player {
        return false;
    }
    // CR 113.6 + CR 113.6b: A permanent's abilities function only on the battlefield by
    // default; an ability that states which zones it functions in (activation_zone, e.g.
    // Hand/Graveyard mana abilities) functions only from those zones.
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if obj.zone != required_zone {
        return false;
    }
    // CR 106.12 + CR 602.5a: A tap-cost mana ability requires an untapped source.
    // Gated on has_tap_component so no-tap sacrifice altars stay activatable while tapped.
    if mana_sources::has_tap_component(&ability_def.cost) && obj.tapped {
        return false;
    }
    // CR 101.2 + CR 107.5 + CR 601.2h + CR 602.2b: a "can't become tapped"
    // source can't pay a tap-cost mana ability. A {Q} untap-cost ability is
    // unaffected — untapping is governed by `StaticMode::CantUntap`.
    if mana_sources::has_tap_component(&ability_def.cost)
        && crate::game::restrictions::object_cant_tap(state, source_id)
    {
        return false;
    }
    // CR 107.6: A {Q}-cost mana ability requires a currently-tapped source — an
    // already-untapped permanent can't be untapped to pay the cost (Pili-Pala).
    if mana_sources::has_untap_component(&ability_def.cost) && !obj.tapped {
        return false;
    }
    // CR 302.6 + CR 602.5a: a {T}- or {Q}-cost mana ability on a creature that
    // hasn't been controlled since the start of its controller's most recent turn
    // can't be activated (CR 302.6 names both symbols). Haste /
    // CanActivateAbilitiesAsThoughHaste lift it via the shared predicate.
    if (mana_sources::has_tap_component(&ability_def.cost)
        || mana_sources::has_untap_component(&ability_def.cost))
        && super::restrictions::summoning_sick_for_tap_ability(state, obj)
    {
        return false;
    }
    // CR 602.5: CantBeActivated (City of Solitude class) blocks activation.
    // CR 604.1: gated existence check hoisted across the board sweep — the
    // per-source full-battlefield scan only runs when such a static exists.
    if gates.has_cant_be_activated
        && super::casting::is_blocked_by_cant_be_activated(state, player, source_id, ability_def)
    {
        return false;
    }
    // CR 602.5 + CR 117.1b: CantActivateDuring blocks activation this turn.
    if gates.has_cant_activate_during
        && super::casting::is_blocked_by_cant_activate_during(state, player, ability_def)
    {
        return false;
    }
    // CR 604 + CR 605.3b: Static activation restrictions must currently hold.
    if super::restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )
    .is_err()
    {
        return false;
    }
    // CR 605.3a + CR 601.2h: The mana sub-cost (pool + choice-of-object) must be
    // currently payable. is_payable_for_mana_ability's Mana arm uses auto_tap with
    // require_current_payability=false, so it does not recurse here.
    if let Some(cost) = &ability_def.cost {
        if !cost.is_payable_for_mana_ability(state, player, source_id, ability_index) {
            return false;
        }
    }
    true
}

/// CR 605.3a + CR 106.12 + CR 107.6 + CR 701.21a: True when the full-state
/// legality clone in [`can_activate_mana_ability_now_gated`] would only
/// re-derive an answer the non-simulating readiness gate has already settled.
///
/// Two disjoint shapes qualify:
/// * [`mana_sources::cost_conclusively_payable_by_cheap_gate`] — no cost, or a
///   cost built solely from the `{T}`/`{Q}` symbols. Unchanged.
/// * A whole-tree choice-free cost that sacrifices exactly the ability's own
///   source ([`mana_sources::has_unambiguous_self_sacrifice_component`]) —
///   Treasure's `{T}, Sacrifice this token` (CR 111.10a) and Gold's tapless
///   `Sacrifice this token` (CR 111.10c). `SacrificeRequirement::Aggregate`,
///   `Count { count: n > 1 }` and every non-`SelfRef` target are excluded BY
///   CONSTRUCTION by that predicate's `Count { count: 1 }` / `SelfRef` match —
///   a non-self sacrifice may have no legal victim, so its simulation is
///   load-bearing and must not be skipped.
///
/// The second shape is the first MULTI-component cost the engine answers
/// without simulating, so the two divergences an earlier component can
/// introduce are guarded explicitly. Both guards are conservative: a `true`
/// declines the fast path and falls through to the unchanged simulation, so a
/// spurious guard costs performance, never correctness. Note that Guard 2's
/// granularity is the whole board rather than this source: `static_kind_present`
/// is a board-global `StaticModeKind` presence read (CR 604.1), so a single
/// `CantPayCost` permanent anywhere on the battlefield (Yasharn, Impeccable Sire)
/// returns EVERY mana source to the clone path, not only the sources a
/// prohibition could actually name.
///
/// Two divergences deliberately need NO guard:
/// * CR 616.1 — a replacement on the sacrifice's battlefield -> graveyard move
///   makes `sacrifice_permanent` return `NeedsReplacementChoice`, which the
///   self-sacrifice payment arm maps to `Ok(ManaAbilityPaymentProgress::Paused)`,
///   so the simulation returns `Ok` and reports the same `true` this path does.
/// * CR 608.2h — the production tail runs after the source has left the
///   battlefield, so a source-referential produced amount (Lotus Blossom's
///   `CountersOn { scope: Source }`) reads last known information. That changes
///   the amount of mana, never the legality answer: the tail's only two `?`
///   operators are `mana_ability_definition` (rescued by the activation's
///   `ability_snapshot`) and `resume_mana_ability_root` (infallible for the
///   `Priority` resume the legality simulation uses).
fn legality_simulation_is_redundant(
    state: &GameState,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> bool {
    if mana_sources::cost_conclusively_payable_by_cheap_gate(cost) {
        return true;
    }
    mana_sources::has_unambiguous_self_sacrifice_component(cost)
        // CR 601.2g: a permanent already committed to a pending spell's
        // additional sacrifice cost is reserved; paying this ability's cost
        // then errors at `continue_mana_ability_cost_payment_in_node`. Reuses
        // the payment path's own authority rather than re-deriving it.
        && !cost_sacrifices_reserved_source(state, source_id, cost)
        // CR 118.3 + CR 601.2h: the readiness gate evaluated
        // `player_cant_sacrifice_as_cost` on the PRE-payment state, but the
        // payment re-evaluates it after this tree's `{T}` component has
        // already tapped the source, and a prohibition's object filter can
        // read that tapped bit (`FilterProp::Tapped`). O(1) presence read
        // (CR 604.1): a `false` here is precise post-flush, so the two
        // evaluations are provably identical; a `true` declines and simulates.
        && !static_kind_present(state, StaticModeKind::CantPayCost)
}

pub fn can_activate_mana_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    // Single-call entry: compute the activation-prohibition gates once and
    // delegate. Board-wide sweeps use `..._gated` to hoist them across sources.
    let gates = ManaActivationGates::compute(state);
    can_activate_mana_ability_now_gated(
        state,
        player,
        source_id,
        ability_index,
        ability_def,
        &gates,
    )
}

/// Gated variant of [`can_activate_mana_ability_now`]: the caller supplies
/// once-computed [`ManaActivationGates`] so a board-global mana sweep does not
/// re-scan the battlefield for activation-prohibition statics per source.
pub fn can_activate_mana_ability_now_gated(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    gates: &ManaActivationGates,
) -> bool {
    #[cfg(test)]
    MANA_READINESS_CALLS.fetch_add(1, Ordering::Relaxed);

    if !mana_ability_ready_without_simulation_gated(
        state,
        player,
        source_id,
        ability_index,
        ability_def,
        gates,
    ) {
        return false;
    }
    // CR 605.3a + CR 106.12 + CR 107.6: When the cheap gate already conclusively
    // decides payability (no cost, or a {T}/{Q}-only cost whose production +
    // payment path is infallible), skip the full-state-clone legality
    // simulation. Eliminates the mana-display board-sweep clone-storm (Cryptolith
    // Rite granting bare `{T}: Add` to ~700 tokens => ~700 clones/sweep). Mana/
    // resource/composite costs still simulate — the auto-tap affordability
    // witness (CR 601.2g) must not flip UNAVAILABLE->AVAILABLE. CR 111.10a +
    // CR 701.21a: a whole-tree choice-free cost that sacrifices the ability's
    // OWN source (Treasure, Gold, Lotus Petal) is conclusively decided the same
    // way, behind two state-aware guards — see
    // [`legality_simulation_is_redundant`].
    if legality_simulation_is_redundant(state, source_id, &ability_def.cost) {
        return true;
    }
    can_activate_mana_ability_by_simulation(state, player, source_id, ability_index, ability_def)
}

fn can_activate_mana_ability_by_simulation(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    crate::game::perf_counters::record_state_clone_for_legality();
    crate::game::perf_counters::record_mana_readiness_state_clone();
    let mut simulated = state.clone();
    activate_mana_ability(
        &mut simulated,
        source_id,
        player,
        ability_index,
        ability_def,
        &mut Vec::new(),
        ManaAbilityResume::Priority,
        None,
    )
    .is_ok()
}

// CR 701.59: collect-evidence amount inside a (possibly composite) mana-ability cost.
pub(crate) fn collect_evidence_cost_amount(cost: &AbilityCost) -> Option<u32> {
    match cost {
        AbilityCost::CollectEvidence { amount } => Some(*amount),
        AbilityCost::Composite { costs } => costs.iter().find_map(collect_evidence_cost_amount),
        _ => None,
    }
}

// CR 107.3a + CR 702.179f: a mana-ability cost of "Pay X speed".
fn pay_speed_x_cost(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::PaySpeed {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
        } if name == "X" => true,
        AbilityCost::Composite { costs } => costs.iter().any(pay_speed_x_cost),
        _ => false,
    }
}

pub(super) fn advance_mana_ability_activation(
    state: &mut GameState,
    pending: PendingManaAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let ability_def = mana_ability_definition(state, &pending)?;

    // CR 107.3a + CR 601.2b + CR 702.179f: A `Pay X speed` mana-ability cost
    // (Chicago Loop's `Pay X speed: Add X mana in any combination of colors`)
    // requires the player to announce X before any cost is paid or mana is
    // produced. X is bound to BOTH the speed cost and the produced-mana count.
    if pending.chosen_x.is_none() {
        if let Some(cost) = &ability_def.cost {
            if pay_speed_x_cost(cost) {
                // CR 118.3: a player can't pay a cost without the resources to
                // pay it fully, so X is bounded by the player's current speed.
                // CR 702.179f: a player with no speed has speed 0.
                let max = super::speed::effective_speed(state, pending.player) as u32;
                let source_id = pending.source_id;
                let player = pending.player;
                return Ok(WaitingFor::PayAmountChoice {
                    player,
                    resource: PayableResource::Speed,
                    min: 0,
                    max,
                    accumulated: 0,
                    source_id,
                    pending_mana_ability: Some(Box::new(pending)),
                });
            }
        }
    }

    if pending.chosen_discards.is_empty() {
        if let Some((count, cards)) =
            discard_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if cards.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard for mana ability".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::Discard,
                choices: cards,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    if pending.chosen_tappers.is_empty() {
        if let Some((count, creatures)) =
            tap_creature_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if creatures.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough untapped creatures to pay mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::TapCreatures { aggregate: None },
                choices: creatures,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    // CR 117.1 + CR 118.3 + CR 400.7j: Non-self exile as a mana ability cost.
    // Library costs are deterministic top-card payment, so prepare their
    // selected objects and cost-paid snapshot before any mana output prompt.
    if pending.chosen_exiled.is_empty() {
        if let Some(updated) =
            prepare_deterministic_exile_cost_selection(state, &pending, &ability_def.cost)?
        {
            return advance_mana_ability_activation(state, updated, events);
        }
    }

    // CR 117.1 + CR 118.3: Interactive non-self exile costs (Food Chain,
    // Titans' Nest) choose objects before producing mana so the cost-paid
    // object's public characteristics can be captured at payment time.
    if pending.chosen_exiled.is_empty() {
        if let Some((count, zone, cards)) =
            exile_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if cards.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible cards to exile for mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::ExileFromManaZone { zone },
                choices: cards,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    // CR 117.1 + CR 118.3: Non-self sacrifice-from-battlefield as a mana
    // ability cost (Phyrexian Altar class). Surface the player choice before
    // producing mana so the selected permanent is sacrificed as the cost.
    if pending.chosen_sacrificed_battlefield.is_empty() {
        if let Some((count, permanents)) =
            sacrifice_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            let permanents: Vec<ObjectId> = permanents
                .into_iter()
                .filter(|id| !deferred_spell_sacrifice_reserved(state, *id))
                .collect();
            if permanents.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to sacrifice for mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::Sacrifice,
                choices: permanents,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    // CR 605.2 + CR 701.59: "Collect evidence N" in a (possibly composite)
    // mana-ability cost (Cryptex) requires interactively exiling graveyard
    // cards before any mana is produced. Surface the choice via the shared
    // CollectEvidenceChoice prompt, resuming this activation once cards are
    // chosen. Keyed on not-yet-collected (empty selection).
    if pending.collected_evidence.is_empty() {
        if let Some(cost) = &ability_def.cost {
            if let Some(amount) = collect_evidence_cost_amount(cost) {
                // CR 605.2 + CR 605.3b: pay the ability's cost before producing mana.
                if !super::effects::collect_evidence::can_collect_evidence(
                    state,
                    pending.player,
                    amount,
                ) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay collect-evidence cost for mana ability".to_string(),
                    ));
                }
                return Ok(
                    super::effects::collect_evidence::begin_cost_payment_for_mana_ability(
                        state,
                        pending.player,
                        amount,
                        pending,
                    ),
                );
            }
        }
    }

    // CR 107.1c + CR 605.3a: "Remove any number of <type> counters" in a
    // mana-ability cost requires choosing the count before costs are paid and
    // mana is produced.
    if pending.chosen_counter_count.is_none() {
        if let Some(counter_type) = any_number_self_remove_counter_cost(&ability_def.cost) {
            let max = removable_counter_count_for_mana_cost(state, pending.source_id, counter_type);
            return Ok(WaitingFor::PayAmountChoice {
                player: pending.player,
                resource: PayableResource::Counters,
                min: 0,
                max,
                accumulated: 0,
                source_id: pending.source_id,
                pending_mana_ability: Some(Box::new(pending)),
            });
        }
    }

    // CR 605.3a + CR 602.2b + CR 601.2g-h + CR 107.4e: Resolve the mana
    // sub-cost payment before producing any mana or prompting for output
    // choices. If the current pool already offers multiple hybrid assignments,
    // surface `PayManaAbilityMana` so the player picks. If the pool cannot
    // cover the sub-cost yet, fall through to the real payment site, which may
    // activate other mana abilities while paying this activation cost (CR
    // 117.1d / CR 118.2).
    if pending.chosen_mana_payment.is_none() {
        if let Some(sub_cost) = mana_sub_cost_of(&ability_def.cost) {
            let activation_context = super::casting::activation_payment_context(
                state,
                pending.source_id,
                pending.ability_index,
            );
            let activation_ctx = activation_context.as_payment_context();
            let pool = &state.players[pending.player.0 as usize].mana_pool;
            let plans = enumerate_hybrid_payment_plans(pool, sub_cost, &activation_ctx);
            match plans.len() {
                0 if {
                    let excluded_sources = std::collections::HashSet::from([pending.source_id]);
                    !super::casting::can_pay_ability_mana_cost_after_auto_tap_excluding(
                        state,
                        pending.player,
                        pending.source_id,
                        pending.ability_index,
                        sub_cost,
                        &excluded_sources,
                    )
                } =>
                {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay mana cost for mana ability".to_string(),
                    ));
                }
                0 => {}
                1 => {
                    let mut updated = pending;
                    updated.chosen_mana_payment = Some(plans.into_iter().next().unwrap());
                    return advance_mana_ability_activation(state, updated, events);
                }
                _ => {
                    return Ok(WaitingFor::PayManaAbilityMana {
                        player: pending.player,
                        options: plans,
                        pending_mana_ability: Box::new(pending),
                    });
                }
            }
        }
    }

    // CR 601.2h + CR 602.2b + CR 616.1: The activation owns a serialized
    // component cursor while any replaceable cost move is paused. It never
    // re-enters this choice-discovery prefix after the player answers a
    // replacement choice, so paid components and selected objects stay paid.
    let cost_event_start = events.len();
    continue_mana_ability_cost_payment(
        state,
        pending,
        mana_ability_cost_cursor(
            &ability_def.cost,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::Interactive,
            None,
        ),
        events,
        cost_event_start,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManaAbilityPaymentProgress {
    Complete,
    Paused,
}

fn mana_ability_definition(
    state: &GameState,
    pending: &PendingManaAbility,
) -> Result<AbilityDefinition, EngineError> {
    state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| {
            pending
                .ability_index
                .and_then(|index| obj.abilities.get(index))
        })
        .cloned()
        .or_else(|| pending.ability_snapshot.clone())
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))
}

/// CR 603.2 + CR 603.3b: Build the child-facing snapshot of a live synchronous
/// parent frame. Only this clone's ledger gains the parent's current unscanned
/// suffix; the live parent cursor keeps its own `cost_event_start` and its
/// events remain in the reducer vector, so a synchronous child that drops the
/// snapshot cannot duplicate the root's eventual scan. Preparation runs at every
/// child entry, so a later pausing sibling inherits the parent prefix plus all
/// earlier synchronously completed siblings in chronological order.
fn parent_snapshot_with_current_cost_events(
    parent: &ManaAbilityCostParent,
    events: &[GameEvent],
) -> ManaAbilityCostParent {
    debug_assert!(
        matches!(
            parent.lifecycle,
            ManaAbilityCostParentLifecycle::Synchronous
        ),
        "only a live synchronous parent may be augmented; a suspended parent's prefix is already durable"
    );
    debug_assert!(
        parent.current_action_event_start <= events.len(),
        "parent event marker must index the live reducer event vector"
    );
    let mut prepared = parent.clone();
    if matches!(
        parent.lifecycle,
        ManaAbilityCostParentLifecycle::Synchronous
    ) {
        let start = parent.current_action_event_start.min(events.len());
        prepared
            .cursor
            .deferred_cost_events
            .extend_from_slice(&events[start..]);
    }
    prepared
}

fn mana_ability_cost_cursor(
    cost: &Option<AbilityCost>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    resolution_mode: ManaAbilityCostResolutionMode,
    parent: Option<&ManaAbilityCostParent>,
) -> ManaAbilityCostCursor {
    let mut remaining = Vec::new();
    if let Some(cost) = cost {
        append_mana_ability_cost_components(cost, &mut remaining);
    }
    let mut excluded_sources: Vec<_> = excluded_sources.iter().copied().collect();
    excluded_sources.sort_unstable_by_key(|source_id| source_id.0);
    ManaAbilityCostCursor {
        remaining,
        remaining_life_payments: Vec::new(),
        resolution_mode,
        excluded_sources,
        sub_cost_demand: sub_cost_demand.copied(),
        next_tapper: 0,
        next_discard: 0,
        next_exiled: 0,
        next_sacrificed: 0,
        selected_exile_remaining: None,
        selected_sacrifice_remaining: None,
        // CR 603.2 + CR 603.3b: A nested child starts with an empty frame-local
        // ledger. Its parent snapshot retains every ancestor-owned event
        // opaquely until a suspended child moves its own events upward.
        deferred_cost_events: Vec::new(),
        current_action_deferred_start: 0,
        parent: parent.cloned().map(Box::new),
    }
}

fn append_mana_ability_cost_components(cost: &AbilityCost, remaining: &mut Vec<AbilityCost>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for cost in costs {
                append_mana_ability_cost_components(cost, remaining);
            }
        }
        cost => remaining.push(cost.clone()),
    }
}

fn promote_cost_move_resume(pending: &mut PendingManaAbility) {
    if let Some(resume) = pending.cost_move_resume.take() {
        pending.resume = resume;
    }
}

fn promote_cost_move_resume_chain(
    pending: &mut PendingManaAbility,
    parent: Option<&mut ManaAbilityCostParent>,
) {
    promote_cost_move_resume(pending);
    if let Some(parent) = parent {
        promote_cost_move_resume_chain(&mut parent.pending, parent.cursor.parent.as_deref_mut());
    }
}

fn suspend_mana_ability_parent_chain(parent: Option<&mut ManaAbilityCostParent>) {
    if let Some(parent) = parent {
        parent.lifecycle = ManaAbilityCostParentLifecycle::Suspended;
        suspend_mana_ability_parent_chain(parent.cursor.parent.as_deref_mut());
    }
}

/// Only a pre-delivery replacement-ordering pause may replace the active prompt
/// with `ReplacementChoice`. A post-delivery substitute can keep a replacement
/// record while it waits on its own prompt, so it has no ordering player and
/// must remain visible.
fn pause_pre_delivery_mana_cost_replacement_choice(
    state: &mut GameState,
    choice_player: Option<PlayerId>,
) {
    if let Some(choice_player) = choice_player.filter(|_| state.pending_replacement.is_some()) {
        super::costs::pause_cost_payment_for_replacement_choice(state, choice_player);
    }
}

fn pause_mana_ability_cost_payment(
    state: &mut GameState,
    pre_delivery_choice_player: Option<PlayerId>,
    mut pending: PendingManaAbility,
    mut cursor: ManaAbilityCostCursor,
    events: &[GameEvent],
    cost_event_start: usize,
) {
    // CR 605.3b + CR 616.1: Promote the typed root only on the paused path.
    // In particular, a synchronously completed auto-tap must return normally
    // so its caller can spend the mana rather than recursively retrying.
    promote_cost_move_resume_chain(&mut pending, cursor.parent.as_deref_mut());
    // CR 605.3b + CR 605.3c: Only a pause unwinds the parent's synchronous
    // call frame. Mark the complete parent chain so the resumed child, and no
    // synchronously completed child, takes ownership of continuing it.
    suspend_mana_ability_parent_chain(cursor.parent.as_deref_mut());
    cursor.current_action_deferred_start = cursor.deferred_cost_events.len();
    cursor
        .deferred_cost_events
        .extend_from_slice(&events[cost_event_start..]);
    state.pending_cost_move_resume = Some(PendingCostMoveResume::ManaAbilityPayment {
        pending: Box::new(pending),
        cursor,
    });
    pause_pre_delivery_mana_cost_replacement_choice(state, pre_delivery_choice_player);
}

fn mana_ability_cursor_after_current_component(
    cursor: &ManaAbilityCostCursor,
) -> ManaAbilityCostCursor {
    let mut resumed = cursor.clone();
    resumed.remaining.remove(0);
    resumed
}

fn ensure_mana_ability_selection_cursor_consumed(
    pending: &PendingManaAbility,
    cursor: &ManaAbilityCostCursor,
) -> Result<(), EngineError> {
    if cursor.next_tapper != pending.chosen_tappers.len() {
        return Err(EngineError::InvalidAction(
            "Too many creatures selected for mana ability cost".to_string(),
        ));
    }
    if cursor.next_exiled != pending.chosen_exiled.len() {
        return Err(EngineError::InvalidAction(
            "Too many cards selected for mana ability exile cost".to_string(),
        ));
    }
    if cursor.next_discard != pending.chosen_discards.len() {
        return Err(EngineError::InvalidAction(
            "Too many cards selected for mana ability cost".to_string(),
        ));
    }
    if cursor.next_sacrificed != pending.chosen_sacrificed_battlefield.len() {
        return Err(EngineError::InvalidAction(
            "Too many permanents selected for mana ability sacrifice cost".to_string(),
        ));
    }
    Ok(())
}

/// CR 603.2 + CR 603.3b: Move one suspended child's frame-local trigger-event
/// ledger into its parent without replacing the parent's earlier events.
fn append_suspended_child_cost_events(
    parent: &mut ManaAbilityCostCursor,
    child: &mut ManaAbilityCostCursor,
    current: &[GameEvent],
) {
    parent
        .deferred_cost_events
        .extend(std::mem::take(&mut child.deferred_cost_events));
    parent.deferred_cost_events.extend_from_slice(current);
}

fn advance_mana_ability_selection_cursor(
    cursor: &mut ManaAbilityCostCursor,
    cost: &AbilityCost,
    paid_discard_count: Option<usize>,
) -> Result<(), EngineError> {
    match cost {
        AbilityCost::TapCreatures { requirement, .. } => {
            cursor.next_tapper += requirement.fixed_count().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Aggregate-power tap cost is not valid for a mana ability".to_string(),
                )
            })? as usize;
        }
        AbilityCost::Discard { self_scope, .. } if !self_scope.is_source_card() => {
            cursor.next_discard += paid_discard_count.expect(
                "mana ability discard count must be captured before moving the selected cards",
            );
        }
        AbilityCost::Sacrifice(cost) if !matches!(cost.target, TargetFilter::SelfRef) => {
            let crate::types::ability::SacrificeRequirement::Count { count } = cost.requirement
            else {
                return Err(EngineError::InvalidAction(
                    "Unsupported sacrifice cost requirement for mana ability".to_string(),
                ));
            };
            cursor.next_sacrificed += count as usize;
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pay_selected_mana_ability_exile_cost(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: &mut ManaAbilityCostCursor,
    count: u32,
    zone: Option<Zone>,
    filter: Option<&TargetFilter>,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<ManaAbilityPaymentProgress, EngineError> {
    let effective_zone = exile_cost_effective_zone(zone, filter);
    if effective_zone == Zone::Library && filter.is_some() {
        return Err(EngineError::InvalidAction(
            "Unsupported filtered library exile cost for mana ability".to_string(),
        ));
    }
    if cursor.selected_exile_remaining.is_none() {
        let end = cursor.next_exiled + count as usize;
        let selected = pending
            .chosen_exiled
            .get(cursor.next_exiled..end)
            .ok_or_else(|| {
                EngineError::InvalidAction(
                    "Missing exiled card selection for mana ability".to_string(),
                )
            })?;
        if contains_duplicate_object_id(selected) {
            return Err(EngineError::InvalidAction(
                "Cannot exile the same card more than once for a mana ability cost".to_string(),
            ));
        }
        let legal = eligible_exile_cost_objects(
            state,
            pending.player,
            pending.source_id,
            effective_zone,
            filter,
            count,
        );
        if effective_zone == Zone::Library {
            if selected != legal {
                return Err(EngineError::ActionNotAllowed(
                    "Selected cards are no longer on top of your library".to_string(),
                ));
            }
        } else if selected.iter().any(|object_id| {
            deferred_spell_sacrifice_reserved(state, *object_id) || !legal.contains(object_id)
        }) {
            return Err(EngineError::ActionNotAllowed(
                "Selected card does not match the exile cost".to_string(),
            ));
        }
        cursor.next_exiled = end;
        cursor.selected_exile_remaining = Some(selected.to_vec());
    }

    while let Some(object_id) = cursor
        .selected_exile_remaining
        .as_ref()
        .and_then(|remaining| remaining.first())
        .copied()
    {
        if object_id == pending.source_id {
            return Err(EngineError::ActionNotAllowed(
                "Source cannot satisfy its own exile cost".to_string(),
            ));
        }
        cursor
            .selected_exile_remaining
            .as_mut()
            .expect("selected exile cursor was checked above")
            .remove(0);
        match zone_pipeline::move_object(
            state,
            ZoneMoveRequest::cost(object_id, Zone::Exile, pending.source_id),
            events,
        ) {
            ZoneMoveResult::Done => {}
            ZoneMoveResult::NeedsChoice(choice_player) => {
                pause_mana_ability_cost_payment(
                    state,
                    Some(choice_player),
                    pending,
                    cursor.clone(),
                    events,
                    cost_event_start,
                );
                return Ok(ManaAbilityPaymentProgress::Paused);
            }
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
                unreachable!("a cost move to Exile cannot require Aura attachment")
            }
        }
    }
    cursor.selected_exile_remaining = None;
    Ok(ManaAbilityPaymentProgress::Complete)
}

/// CR 601.2h + CR 605.3b + CR 616.1: Pay a selected sacrifice component from
/// the mana-ability cursor. The selected list is consumed before proposing each
/// sacrifice, so a replacement-choice resume cannot re-sacrifice the object
/// whose move the replacement action just settled.
#[allow(clippy::too_many_arguments)]
fn pay_selected_mana_ability_sacrifice_cost(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: &mut ManaAbilityCostCursor,
    count: u32,
    filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<ManaAbilityPaymentProgress, EngineError> {
    if cursor.selected_sacrifice_remaining.is_none() {
        let end = cursor.next_sacrificed + count as usize;
        let selected = pending
            .chosen_sacrificed_battlefield
            .get(cursor.next_sacrificed..end)
            .ok_or_else(|| {
                EngineError::InvalidAction(
                    "Missing sacrificed permanent selection for mana ability".to_string(),
                )
            })?;
        if contains_duplicate_object_id(selected) {
            return Err(EngineError::InvalidAction(
                "Cannot sacrifice the same permanent more than once for a mana ability cost"
                    .to_string(),
            ));
        }
        cursor.next_sacrificed = end;
        cursor.selected_sacrifice_remaining = Some(selected.to_vec());
    }

    while let Some(object_id) = cursor
        .selected_sacrifice_remaining
        .as_ref()
        .and_then(|remaining| remaining.first())
        .copied()
    {
        cursor
            .selected_sacrifice_remaining
            .as_mut()
            .expect("selected sacrifice cursor was checked above")
            .remove(0);
        match sacrifice_selected_permanent_for_mana_cost(
            state,
            pending.source_id,
            pending.player,
            object_id,
            filter,
            events,
        )? {
            sacrifice::SacrificeOutcome::Complete => {}
            sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                pause_mana_ability_cost_payment(
                    state,
                    Some(choice_player),
                    pending,
                    cursor.clone(),
                    events,
                    cost_event_start,
                );
                return Ok(ManaAbilityPaymentProgress::Paused);
            }
        }
    }
    cursor.selected_sacrifice_remaining = None;
    Ok(ManaAbilityPaymentProgress::Complete)
}

fn pay_mana_ability_cost_component(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: &mut ManaAbilityCostCursor,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<ManaAbilityPaymentProgress, EngineError> {
    match cost {
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone,
            count: 1,
        } => {
            let required_zone = zone.unwrap_or(Zone::Battlefield);
            let source = state.objects.get(&pending.source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exile cost".to_string())
            })?;
            if source.zone != required_zone {
                return Err(EngineError::ActionNotAllowed(format!(
                    "Cannot exile from {:?}: source is not in that zone",
                    required_zone
                )));
            }
            match zone_pipeline::move_object(
                state,
                ZoneMoveRequest::cost(pending.source_id, Zone::Exile, pending.source_id),
                events,
            ) {
                ZoneMoveResult::Done => Ok(ManaAbilityPaymentProgress::Complete),
                ZoneMoveResult::NeedsChoice(choice_player) => {
                    pause_mana_ability_cost_payment(
                        state,
                        Some(choice_player),
                        pending,
                        mana_ability_cursor_after_current_component(cursor),
                        events,
                        cost_event_start,
                    );
                    Ok(ManaAbilityPaymentProgress::Paused)
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    unreachable!("a cost move to Exile cannot require Aura attachment")
                }
            }
        }
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } if !matches!(filter, Some(TargetFilter::SelfRef)) => {
            pay_selected_mana_ability_exile_cost(
                state,
                pending,
                cursor,
                *count,
                *zone,
                filter.as_ref(),
                events,
                cost_event_start,
            )
        }
        AbilityCost::Sacrifice(cost)
            if matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1) =>
        {
            if deferred_spell_sacrifice_reserved(state, pending.source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "This permanent is already committed to a spell sacrifice cost".to_string(),
                ));
            }
            if super::static_abilities::player_cant_sacrifice_as_cost(
                state,
                pending.player,
                pending.source_id,
            ) {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot sacrifice this permanent as a cost".to_string(),
                ));
            }
            match sacrifice::sacrifice_permanent(state, pending.source_id, pending.player, events)?
            {
                sacrifice::SacrificeOutcome::Complete => Ok(ManaAbilityPaymentProgress::Complete),
                sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                    pause_mana_ability_cost_payment(
                        state,
                        Some(choice_player),
                        pending,
                        mana_ability_cursor_after_current_component(cursor),
                        events,
                        cost_event_start,
                    );
                    Ok(ManaAbilityPaymentProgress::Paused)
                }
            }
        }
        AbilityCost::Sacrifice(cost)
            if !matches!(cost.target, TargetFilter::SelfRef)
                && matches!(
                    cost.requirement,
                    crate::types::ability::SacrificeRequirement::Count { .. }
                ) =>
        {
            let crate::types::ability::SacrificeRequirement::Count { count } = cost.requirement
            else {
                unreachable!("guarded above");
            };
            pay_selected_mana_ability_sacrifice_cost(
                state,
                pending,
                cursor,
                count,
                &cost.target,
                events,
                cost_event_start,
            )
        }
        cost if is_self_contained_mana_subcost(cost) => {
            match super::costs::pay_ability_cost_for_activation(
                state,
                pending.player,
                pending.source_id,
                cost,
                pending.ability_index,
                events,
            )? {
                super::costs::PaymentOutcome::Paid => Ok(ManaAbilityPaymentProgress::Complete),
                super::costs::PaymentOutcome::Paused { .. } => {
                    let Some(PendingCostMoveResume::Cast { .. }) =
                        state.pending_cost_move_resume.take()
                    else {
                        unreachable!(
                            "a paused delegated mana-ability cost must retain the activation cost move"
                        );
                    };
                    let pre_delivery_choice_player = state
                        .pending_replacement
                        .is_some()
                        .then(|| state.waiting_for.acting_player())
                        .flatten();
                    pause_mana_ability_cost_payment(
                        state,
                        pre_delivery_choice_player,
                        pending,
                        mana_ability_cursor_after_current_component(cursor),
                        events,
                        cost_event_start,
                    );
                    Ok(ManaAbilityPaymentProgress::Paused)
                }
                super::costs::PaymentOutcome::Failed { reason } => {
                    Err(EngineError::ActionNotAllowed(reason.reason))
                }
            }
        }
        cost => {
            // CR 601.2h: A dynamic discard cost is measured before its cards
            // leave the hand, so retain that paid count for the cursor.
            let paid_discard_count = match cost {
                AbilityCost::Discard {
                    count, self_scope, ..
                } if !self_scope.is_source_card() => Some(
                    super::quantity::resolve_quantity(
                        state,
                        count,
                        pending.player,
                        pending.source_id,
                    )
                    .max(0) as usize,
                ),
                _ => None,
            };
            let excluded_sources = cursor.excluded_sources.iter().copied().collect();
            let mut tappers = pending
                .chosen_tappers
                .iter()
                .copied()
                .skip(cursor.next_tapper);
            let mut discards = pending
                .chosen_discards
                .iter()
                .copied()
                .skip(cursor.next_discard);
            let mut sacrificed = pending
                .chosen_sacrificed_battlefield
                .iter()
                .copied()
                .skip(cursor.next_sacrificed);
            // CR 605.3b + CR 605.3c: A nested source that pauses while
            // funding this Mana component must retain this exact parent cursor.
            // The child's frame-local deferred ledger starts empty; ancestor
            // events stay opaque in this parent snapshot until upward handoff.
            let parent_cursor = cursor.clone();
            let parent = ManaAbilityCostParent {
                pending: Box::new(pending.clone()),
                cursor: Box::new(parent_cursor),
                lifecycle: ManaAbilityCostParentLifecycle::Synchronous,
                // CR 603.2 + CR 603.3b: Record where this parent frame's own
                // unscanned events begin so each nested child entry can prepare
                // a snapshot carrying that prefix plus any earlier synchronous
                // sibling. The live cursor and its `cost_event_start` are not
                // modified.
                current_action_event_start: cost_event_start,
            };
            let prior_waiting_for = state.waiting_for.clone();
            let component_progress = match pay_mana_ability_cost_with_choices(
                state,
                pending.source_id,
                pending.player,
                pending.ability_index,
                &Some(cost.clone()),
                events,
                &mut tappers,
                &mut discards,
                &mut sacrificed,
                pending.chosen_mana_payment.as_deref(),
                pending.chosen_counter_count,
                pending.chosen_x,
                &excluded_sources,
                cursor.sub_cost_demand.as_ref(),
                Some(&parent),
            ) {
                Ok(progress) => progress,
                // CR 605.3b + CR 605.3c + CR 616.1: A nested mana source
                // paused on a replacement-aware cost. Its serialized cursor
                // owns this exact parent; do not turn that valid suspension
                // into a failed parent mana payment.
                Err(_) if super::casting::mana_ability_cost_payment_is_paused(state) => {
                    return Ok(ManaAbilityPaymentProgress::Paused)
                }
                Err(error) => return Err(error),
            };
            advance_mana_ability_selection_cursor(cursor, cost, paid_discard_count)?;
            let choice_player = match &component_progress {
                ManaAbilityCostComponentProgress::Complete => None,
                ManaAbilityCostComponentProgress::Paused {
                    remaining_life_payments,
                    choice_player,
                } => {
                    cursor
                        .remaining_life_payments
                        .clone_from(remaining_life_payments);
                    *choice_player
                }
            };
            // CR 614.6: The cost itself may be paid while a replacement's
            // interactive substitute remains unresolved. Advance the cursor
            // exactly once, then park the mana ability until that substitute
            // terminally leaves the resolution stack.
            if matches!(
                component_progress,
                ManaAbilityCostComponentProgress::Paused { .. }
            ) || state.waiting_for != prior_waiting_for
            {
                pause_mana_ability_cost_payment(
                    state,
                    choice_player,
                    pending,
                    cursor.clone(),
                    events,
                    cost_event_start,
                );
                return Ok(ManaAbilityPaymentProgress::Paused);
            }
            Ok(ManaAbilityPaymentProgress::Complete)
        }
    }
}

fn finish_mana_ability_cost_payment(
    state: &mut GameState,
    mut pending: PendingManaAbility,
    mut cursor: ManaAbilityCostCursor,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<WaitingFor, EngineError> {
    let resolves_automatically = matches!(
        cursor.resolution_mode,
        ManaAbilityCostResolutionMode::AutoResolved
    );
    let has_deferred_cost_events = !cursor.deferred_cost_events.is_empty();
    let is_ultimate_root = cursor.parent.is_none();
    // CR 605.3b + CR 605.4a: The `AutoResolved` direct resolver
    // (`resolve_mana_ability_excluding`) is not an action's completed root mana
    // frame — it is the synchronous auto-tap/probe entry, whose events belong to
    // the OUTER cost owner already on the Rust call stack and whose caller
    // restores its own `waiting_for`. It keeps its historical default-output
    // semantics: with an empty durable ledger it performs no collection at all,
    // exactly as baseline. Only a frame that already owns replacement-paused
    // events settles from this path.
    let settles_completed_frame =
        is_ultimate_root && (has_deferred_cost_events || !resolves_automatically);
    if matches!(
        cursor.parent.as_deref(),
        Some(ManaAbilityCostParent {
            lifecycle: ManaAbilityCostParentLifecycle::Synchronous,
            ..
        })
    ) {
        debug_assert!(
            cursor.deferred_cost_events.is_empty(),
            "a synchronous mana child cannot own deferred ancestor events"
        );
    }
    let parent = cursor
        .parent
        .take()
        .filter(|parent| matches!(parent.lifecycle, ManaAbilityCostParentLifecycle::Suspended));
    let ability_def = mana_ability_definition(state, &pending)?;
    if !resolves_automatically && pending.color_override.is_none() {
        let resolved_for_prompt = resolved_mana_ability_for_current_state(
            state,
            pending.source_id,
            pending.player,
            &ability_def,
            pending.chosen_x,
            pending.cost_paid_object.clone(),
        );
        if let Some(choice) = mana_choice_prompt(
            &resolved_for_prompt.effect,
            state,
            pending.source_id,
            Some(&resolved_for_prompt),
            Some(&resolved_for_prompt),
        ) {
            if matches!(choice, ManaChoicePrompt::SingleColor { .. })
                && cost_resolves_without_choice(&ability_def.cost)
            {
                pending.batch_siblings =
                    batch_eligible_siblings(state, pending.player, pending.source_id, &ability_def);
            }
            let choice_player = pending.player;
            let context = ManaChoiceContext::ManaAbility(Box::new(pending));
            let resume = WaitingFor::ChooseManaColor {
                player: choice_player,
                choice: choice.clone(),
                context: context.clone(),
            };
            if settles_completed_frame {
                // CR 603.2 + CR 603.3b + CR 605.3b + CR 605.4a: A mana-color
                // choice returns before the ordinary post-action pipeline
                // runs, so the already-paid cost range needs its one normal
                // trigger collection here — through the SAME typed
                // completed-frame seam the durable-ledger root uses, whether
                // or not that ledger is empty. The empty-ledger shape used to
                // fall through to a bare `process_triggers`, which dispatched
                // an ordinary cost observer separately from the frame's own
                // synthetic reflexive.
                debug_assert!(cursor.parent.is_none());
                if let Some(pause) = collect_completed_mana_frame_events(
                    state,
                    cursor.deferred_cost_events,
                    events,
                    cost_event_start,
                    ManaTriggerFixedPointResume::ColorChoice(Box::new(ManaColorChoiceResume {
                        player: choice_player,
                        choice,
                        context,
                    })),
                ) {
                    return Ok(pause);
                }
                if let Some(order_wf) =
                    super::triggers::preserve_order_triggers_resume(state, resume.clone())
                {
                    return Ok(order_wf);
                }
            }
            return Ok(resume);
        }
    }

    let production_events_start = events.len();
    produce_mana_from_ability(
        state,
        pending.source_id,
        pending.player,
        &ability_def,
        events,
        pending.color_override.clone(),
        pending.chosen_x,
        pending.cost_paid_object,
    );
    if !resolves_automatically {
        complete_mana_ability_activation(
            state,
            pending.source_id,
            pending.ability_index,
            pending.player,
            events,
        );
    }
    // CR 605.4a: A triggered mana ability resolves immediately after the mana
    // ability that triggered it, before the enclosing payment path collects
    // ordinary triggers or resumes its payment prompt.
    super::triggers::resolve_tap_mana_triggers_inline(state, events, production_events_start);
    if let Some(parent) = parent {
        let mut parent_cursor = *parent.cursor;
        // CR 603.2 + CR 603.3b: Move the suspended child's frame-local batch
        // upward without replacing the ancestor's earlier ledger. Append the
        // child's current action exactly once before retrying the parent's
        // still-unpaid Mana component.
        append_suspended_child_cost_events(
            &mut parent_cursor,
            &mut cursor,
            &events[cost_event_start..],
        );
        let parent_event_start = events.len();
        return continue_mana_ability_cost_payment(
            state,
            *parent.pending,
            parent_cursor,
            events,
            parent_event_start,
        );
    }
    // CR 603.2 + CR 603.3b + CR 605.3b + CR 605.4a: EVERY completed root mana
    // frame settles through the one typed seam, whether or not its durable
    // ledger holds replacement-paused events and whatever it resumes to — and it
    // settles **before** `resume_mana_ability_root`, not after.
    //
    // Baseline had two bypasses here. The durable-ledger branch called the
    // typed seam; a `ManaPayment`/`UnlessPayment` resume with an empty ledger
    // called `process_triggers` directly (because the post-action pipeline is
    // guarded by `waiting_for == Priority` and would otherwise drop every
    // already-paid cost observer — Scavenger's Talent, Korvold, Mayhem Devil,
    // ...; #5963); and a `Priority` resume with an empty ledger fell through to
    // the pipeline's own generic scan. The first bypass could dispatch an
    // ordinary cost observer separately from this frame's synthetic reflexive;
    // the second could let the pipeline rediscover events the frame already
    // owns. The typed seam closes both: it journals every live occurrence it
    // claims into `consumed_before_priority_trigger_events`, so the pipeline's
    // scan is narrowed by the journal rather than by excluding the `Priority`
    // resume, and nothing double-fires (e.g. Kilo's becomes-tapped proliferate
    // under a standalone Relic activation).
    //
    // Settlement-before-resume is what makes CR 605.4a hold at a durable-ledger
    // root: this frame's accepted triggered mana must be spendable BY the thing
    // it resumes into (the automatic payment finalizer, the unless-payment
    // poll, the pay-to-end permission), exactly as it already is at the colour
    // and `TapLandForMana` roots. It is also the only ordering at which
    // `pending.resume` still exists, so the frame can name its own root in
    // `ManaTriggerFixedPointResume::Root` for an accepted pause instead of the
    // `Parent` variant, which is factually wrong for a parentless root.
    if settles_completed_frame {
        debug_assert!(cursor.parent.is_none());
        if let Some(pause) = settle_mana_ability_cost_events(
            state,
            std::mem::take(&mut cursor.deferred_cost_events),
            events,
            cost_event_start,
            ManaTriggerFixedPointResume::Root {
                player: pending.player,
                resume: Box::new(pending.resume.clone()),
            },
        ) {
            return Ok(pause);
        }
    }

    let resume = resume_mana_ability_root(state, pending.player, pending.resume, events)?;
    if super::casting::mana_ability_cost_payment_is_paused(state) {
        debug_assert!(is_ultimate_root);
        // A settled frame has nothing left to hand upward: its ledger was taken
        // and its live occurrences are already journaled, so re-deferring them
        // into the next root would let that root's durable segment — which the
        // journal does NOT filter — collect them a second time.
        if !settles_completed_frame {
            defer_cost_events_into_active_mana_root(
                state,
                cursor.deferred_cost_events,
                &events[cost_event_start..],
            );
        }
        return Ok(resume);
    }

    if settles_completed_frame {
        return Ok(
            super::triggers::preserve_order_triggers_resume(state, resume.clone())
                .unwrap_or(resume),
        );
    }

    Ok(resume)
}

/// CR 603.2 + CR 603.3b + CR 605.4a: The typed mana-cost root is the single
/// settlement authority for every event emitted before and after a replacement
/// pause. Deferred events were never scanned by their initiating action; current
/// events are scanned here even when the nominal resume is Priority.
fn settle_mana_ability_cost_events(
    state: &mut GameState,
    deferred: Vec<GameEvent>,
    events: &mut Vec<GameEvent>,
    current_start: usize,
    outer_resume: ManaTriggerFixedPointResume,
) -> Option<WaitingFor> {
    debug_assert!(
        !matches!(outer_resume, ManaTriggerFixedPointResume::Parent),
        "a parentless root must name its own resume, never the Parent variant"
    );
    collect_completed_mana_frame_events(state, deferred, events, current_start, outer_resume)
}

/// CR 603.2 + CR 603.3b + CR 605.4a: prepare the exact durable-plus-unconsumed-live
/// batch for one completed mana frame and drive it through the single
/// classifier/dispatcher fixed point.
///
/// Used for **every** completed mana frame, whether or not its durable ledger is
/// empty, so the no-ledger direct/colour/`TapLandForMana` shapes stop falling
/// through to a separate aggregate scan.
///
/// The historical inline output of the durable segment appears once in the
/// logical batch and once only as a public copy in the returned event vector;
/// the copies are excluded from the live logical segment while their live
/// occurrences are still journaled, so the durable tail and the copied public
/// tail are never concatenated into the trigger batch twice.
pub(crate) fn collect_completed_mana_frame_events(
    state: &mut GameState,
    mut deferred: Vec<GameEvent>,
    events: &mut Vec<GameEvent>,
    current_start: usize,
    outer_resume: ManaTriggerFixedPointResume,
) -> Option<WaitingFor> {
    let deferred_original_len = deferred.len();
    super::triggers::resolve_tap_mana_triggers_inline(state, &mut deferred, 0);
    let historical_copy_start = events.len();
    events.extend(deferred[deferred_original_len..].iter().cloned());
    let historical_copy_end = events.len();
    super::triggers::resolve_tap_mana_triggers_inline(state, events, current_start);

    // One chronological logical batch: the durable segment exactly once, then
    // every live event this frame owns that an earlier synchronous child or a
    // completed resume prefix has not already claimed.
    let consumed = state.consumed_before_priority_trigger_events.clone();
    let live_indices: Vec<usize> = (current_start..events.len())
        .filter(|index| !(historical_copy_start..historical_copy_end).contains(index))
        .filter(|index| {
            let occurrence = super::triggers::trigger_event_occurrence(events, *index);
            !consumed
                .iter()
                .any(|claimed| claimed.event == events[*index] && claimed.occurrence == occurrence)
        })
        .collect();
    let mut batch = deferred;
    batch.extend(live_indices.iter().map(|index| events[*index].clone()));

    let live_end = events.len();
    let pause = super::triggers::collect_mana_action_trigger_batch(state, &batch, outer_resume);

    // Only after the combined collection result is durably owned: claim every
    // exact live occurrence this frame is responsible for, by full-action index.
    let occurrences = (current_start..live_end)
        .map(
            |index| crate::game::triggers::ConsumedTriggerEventOccurrence {
                event: events[index].clone(),
                occurrence: super::triggers::trigger_event_occurrence(events, index),
            },
        )
        .collect();
    super::triggers::resolve_and_apply_trigger_collection(
        state,
        crate::types::resolved_commands::ResolvedTriggerCollection::ConsumeBeforePriority {
            occurrences,
        },
    )
    .expect(
        "mana-ability cost-settlement consumed-before-priority trigger journal cause must be live",
    );
    pause
}

/// CR 603.2 + CR 603.3b: A parent payment can immediately pause again after
/// its child completes. Transfer the child's unscanned batch into that next
/// typed root so it remains exactly-once owned rather than being dropped.
fn defer_cost_events_into_active_mana_root(
    state: &mut GameState,
    deferred: Vec<GameEvent>,
    current: &[GameEvent],
) {
    let Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) =
        state.pending_cost_move_resume.as_mut()
    else {
        return;
    };
    let local_start = cursor
        .current_action_deferred_start
        .min(cursor.deferred_cost_events.len());
    let local_events = cursor.deferred_cost_events.split_off(local_start);
    let inherited_current_len = current.len().saturating_sub(local_events.len());
    cursor.deferred_cost_events.extend(deferred);
    cursor
        .deferred_cost_events
        .extend_from_slice(&current[..inherited_current_len]);
    cursor.current_action_deferred_start = cursor.deferred_cost_events.len();
    cursor.deferred_cost_events.extend(local_events);
}

/// CR 605.4a: convert one completed fixed point's typed outer continuation back
/// into the suspended mana frame's own wait, exactly once.
///
/// This is the resumed-occurrence counterpart of the tails
/// `finish_mana_ability_cost_payment` and `handle_choose_mana_color` run
/// synchronously, and it mirrors them exactly — including the paused-payment
/// early exit and the `preserve_order_triggers_resume` wrap. It never re-defers
/// a cost-event ledger: the frame that installed this continuation already took
/// its ledger and journaled its live occurrences before pausing.
pub(crate) fn resume_settled_mana_frame(
    state: &mut GameState,
    outer_resume: ManaTriggerFixedPointResume,
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EngineError> {
    match outer_resume {
        // A nested child frame's suspended parent cursor is the authority; no
        // wait is reconstructed here.
        ManaTriggerFixedPointResume::Parent => Ok(None),
        ManaTriggerFixedPointResume::Root { player, resume } => {
            let resumed = resume_mana_ability_root(state, player, *resume, events)?;
            if super::casting::mana_ability_cost_payment_is_paused(state) {
                return Ok(Some(resumed));
            }
            Ok(Some(
                super::triggers::preserve_order_triggers_resume(state, resumed.clone())
                    .unwrap_or(resumed),
            ))
        }
        ManaTriggerFixedPointResume::ColorChoice(choice) => {
            let ManaColorChoiceResume {
                player,
                choice,
                context,
            } = *choice;
            let resume = WaitingFor::ChooseManaColor {
                player,
                choice,
                context,
            };
            Ok(Some(
                super::triggers::preserve_order_triggers_resume(state, resume.clone())
                    .unwrap_or(resume),
            ))
        }
    }
}

pub(crate) fn resume_mana_ability_root(
    state: &mut GameState,
    mana_source_controller: PlayerId,
    resume: ManaAbilityResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match resume {
        ManaAbilityResume::EffectPayCost {
            payer,
            return_to,
            ability,
            cost,
        } => match super::costs::pay_ability_cost_for_resolution(
            state,
            payer,
            cost.as_ref(),
            ability.as_ref(),
            events,
        )? {
            super::costs::PaymentOutcome::Paid => {
                super::effects::resolve_effect_pay_cost_rider(state, ability.as_ref(), events)
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    Ok(WaitingFor::Priority { player: return_to })
                } else {
                    Ok(state.waiting_for.clone())
                }
            }
            super::costs::PaymentOutcome::Paused { .. } => Ok(state.waiting_for.clone()),
            super::costs::PaymentOutcome::Failed { .. } => {
                state.cost_payment_failed_flag = true;
                super::effects::resolve_effect_pay_cost_rider(state, ability.as_ref(), events)
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    Ok(WaitingFor::Priority { player: return_to })
                } else {
                    Ok(state.waiting_for.clone())
                }
            }
        },
        ManaAbilityResume::PhyrexianCastPayment { caster, choices } => {
            super::casting_costs::finalize_mana_payment_with_phyrexian_choices(
                state, caster, &choices, events,
            )
        }
        ManaAbilityResume::FinalizePendingManaPayment { player } => {
            super::casting_costs::finalize_automatic_mana_payment(state, player, events)
        }
        ManaAbilityResume::CompanionToHand { player, cost } => {
            super::companion::resume_companion_to_hand_payment(state, player, cost, events)
        }
        // CR 116.2b + CR 605.3b: NOT compiler-forced either — the `resume =>`
        // catch-all below would route a paused turn-face-up payment into
        // `resume_waiting_for`, which `unreachable!()`s for this family.
        ManaAbilityResume::TurnFaceUp {
            player,
            object_id,
            cost,
            announced_x,
        } => super::morph::resume_turn_face_up_payment(
            state,
            player,
            object_id,
            cost,
            announced_x,
            events,
        ),
        // CR 116.2c + CR 605.3b: NOT compiler-forced — the `resume =>` catch-all
        // below would silently route a paused pay-to-end payment into
        // `resume_waiting_for`, which `unreachable!()`s for this family.
        ManaAbilityResume::EndContinuousEffect {
            player,
            group,
            cost,
        } => super::end_continuous_effect::resume_end_continuous_effect_payment(
            state, player, group, cost, events,
        ),
        resume => Ok(resume_waiting_for(mana_source_controller, resume)),
    }
}

/// CR 118.3b + CR 119.4 + CR 616.1: Finish the outer action after a
/// Phyrexian-style life component was paid and its replacement post-effects
/// completed. This path must not call the mana-payment authority again: the
/// selected mana units were already spent before the replacement choice
/// opened.
pub(crate) fn finish_mana_root_after_deferred_life_payment(
    state: &mut GameState,
    player: PlayerId,
    resume: ManaAbilityResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match resume {
        ManaAbilityResume::Priority => Ok(WaitingFor::Priority { player }),
        ManaAbilityResume::ManaPayment {
            outer_player,
            convoke_mode,
        } => Ok(resume_waiting_for(
            player,
            ManaAbilityResume::ManaPayment {
                outer_player,
                convoke_mode,
            },
        )),
        ManaAbilityResume::ManaSourceSelection {
            player,
            options,
            convoke_mode,
        } => Ok(resume_waiting_for(
            player,
            ManaAbilityResume::ManaSourceSelection {
                player,
                options,
                convoke_mode,
            },
        )),
        ManaAbilityResume::UnlessPayment {
            outer_player,
            cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        } => {
            let remaining_cost = super::costs::remaining_cost_after_paid_mana_prefix(cost.as_ref());
            match remaining_cost {
                Some(cost) => {
                    // CR 118.12 + CR 605.3b + CR 616.1: Only the leading mana
                    // component was committed before the life-replacement
                    // pause. Resume the exact suffix before suppressing the
                    // unless effect.
                    super::engine_payment_choices::continue_unless_payment_after_paid_mana_prefix(
                        state,
                        outer_player.unwrap_or(player),
                        cost,
                        pending_effect,
                        trigger_event,
                        effect_description,
                        remaining,
                        events,
                    )
                }
                None => super::engine_payment_choices::finish_successful_unless_payment(
                    state,
                    pending_effect.as_ref(),
                    &trigger_event,
                    events,
                ),
            }
        }
        ManaAbilityResume::EffectPayCost {
            payer,
            return_to,
            ability,
            cost,
        } => {
            if let Some(remaining_cost) =
                super::costs::remaining_cost_after_paid_mana_prefix(cost.as_ref())
            {
                // CR 118.12 + CR 608.2c: Re-enter resolution through the
                // remaining PayCost suffix. The ordinary continuation authority
                // then pays it before handing off to the original rider.
                super::effects::prepend_remaining_pay_cost_before_parked_rider(
                    state,
                    ability.as_ref(),
                    payer,
                    remaining_cost,
                );
                Ok(WaitingFor::Priority { player: return_to })
            } else {
                super::effects::resolve_effect_pay_cost_rider(state, ability.as_ref(), events)
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    Ok(WaitingFor::Priority { player: return_to })
                } else {
                    Ok(state.waiting_for.clone())
                }
            }
        }
        ManaAbilityResume::CompanionToHand { player, .. } => Ok(
            super::companion::finish_paid_companion_to_hand(state, player, events),
        ),
        ManaAbilityResume::TurnFaceUp {
            player,
            object_id,
            announced_x,
            ..
        // CR 702.37e + CR 107.3d: payment has completed, so commit the
        // turn-face-up action with its already-announced X value.
        } => super::morph::finish_paid_turn_face_up(state, player, object_id, announced_x, events),
        ManaAbilityResume::EndContinuousEffect { player, group, .. } => Ok(
            super::end_continuous_effect::finish_paid_end_continuous_effect(
                state, player, group, events,
            ),
        ),
        ManaAbilityResume::PhyrexianCastPayment { .. }
        | ManaAbilityResume::FinalizePendingManaPayment { .. } => Err(EngineError::InvalidAction(
            "Cast mana payment reached the non-cast deferred-life continuation".to_string(),
        )),
    }
}

fn continue_mana_ability_cost_payment(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: ManaAbilityCostCursor,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<WaitingFor, EngineError> {
    let node = pending
        .rules_execution_node
        .unwrap_or_else(|| state.begin_activated_mana_journal_node(pending.source_id));
    state.with_rules_execution_node(node, |state| {
        continue_mana_ability_cost_payment_in_node(state, pending, cursor, events, cost_event_start)
    })
}

fn continue_mana_ability_cost_payment_in_node(
    state: &mut GameState,
    pending: PendingManaAbility,
    mut cursor: ManaAbilityCostCursor,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<WaitingFor, EngineError> {
    let ability_def = mana_ability_definition(state, &pending)?;
    if cost_sacrifices_reserved_source(state, pending.source_id, &ability_def.cost) {
        return Err(EngineError::ActionNotAllowed(
            "This permanent is already committed to a spell sacrifice cost".to_string(),
        ));
    }
    while let Some(amount) = cursor.remaining_life_payments.first().copied() {
        cursor.remaining_life_payments.remove(0);
        // CR 118.3b + CR 119.4 + CR 616.1: Paying life is a life-loss event,
        // so competing replacements may pause this activation before the
        // payment is applied. Preserve its exact suffix and ordering player.
        match life_costs::pay_life_as_cast_or_activation_cost(state, pending.player, amount, events)
        {
            PayLifeCostResult::Paid { .. } => {}
            PayLifeCostResult::PaidWithDeferredSubstitution { .. } => {
                pause_mana_ability_cost_payment(
                    state,
                    None,
                    pending,
                    cursor,
                    events,
                    cost_event_start,
                );
                return Ok(state.waiting_for.clone());
            }
            PayLifeCostResult::DeferredReplacementChoice { choice_player, .. } => {
                pause_mana_ability_cost_payment(
                    state,
                    Some(choice_player),
                    pending,
                    cursor,
                    events,
                    cost_event_start,
                );
                return Ok(state.waiting_for.clone());
            }
            PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot complete deferred Phyrexian life cost for mana ability".to_string(),
                ));
            }
        }
    }
    while let Some(cost) = cursor.remaining.first().cloned() {
        match pay_mana_ability_cost_component(
            state,
            pending.clone(),
            &mut cursor,
            &cost,
            events,
            cost_event_start,
        )? {
            ManaAbilityPaymentProgress::Complete => {
                cursor.remaining.remove(0);
            }
            ManaAbilityPaymentProgress::Paused => return Ok(state.waiting_for.clone()),
        }
    }
    ensure_mana_ability_selection_cursor_consumed(&pending, &cursor)?;
    finish_mana_ability_cost_payment(state, pending, cursor, events, cost_event_start)
}

/// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: Resume the precise unpaid
/// suffix of a mana ability's activation cost after the interrupted cost move
/// was delivered or fully replaced. Mana production and its inline subchain
/// run only when the cursor has no remaining components.
pub(crate) fn resume_mana_ability_cost_move(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) =
        state.pending_cost_move_resume.take()
    else {
        unreachable!("mana ability cost-move resume requires its typed continuation")
    };
    continue_mana_ability_cost_payment(state, *pending, cursor, events, 0)
}

/// CR 605.3b + CR 605.1a: Run a mana ability's `sub_ability` chain inline.
/// Mana abilities don't use the stack, so non-mana clauses ("This land deals
/// 1 damage to you.") resolve atomically with the mana production. Walks the
/// full chain via `resolve_ability_chain` so nested effects (DealDamage on
/// controller, GainLife, etc.) route through the standard effect handlers.
fn resolve_mana_ability_sub_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    let Some(sub) = ability.sub_ability.as_deref() else {
        return;
    };
    // Errors during the sub-chain are non-fatal — mana has already been
    // added to the pool and the cost has been paid. The damage/life clause
    // of a painland cannot legitimately fail in a well-formed game state.
    let _ = super::effects::resolve_ability_chain(state, sub, events, 0);
}

fn contains_duplicate_object_id(ids: &[ObjectId]) -> bool {
    ids.iter()
        .enumerate()
        .any(|(index, id)| ids[index + 1..].contains(id))
}

#[allow(clippy::too_many_arguments)]
fn pay_mana_ability_cost_with_choices<I, J, L>(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_index: Option<usize>,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
    chosen_tappers: &mut I,
    chosen_discards: &mut J,
    chosen_sacrificed_battlefield: &mut L,
    chosen_hybrid_payment: Option<&[ManaType]>,
    chosen_counter_count: Option<u32>,
    chosen_x: Option<u32>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    parent: Option<&ManaAbilityCostParent>,
) -> Result<ManaAbilityCostComponentProgress, EngineError>
where
    I: Iterator<Item = ObjectId>,
    J: Iterator<Item = ObjectId>,
    L: Iterator<Item = ObjectId>,
{
    if cost_sacrifices_reserved_source(state, source_id, cost) {
        return Err(EngineError::ActionNotAllowed(
            "This permanent is already committed to a spell sacrifice cost".to_string(),
        ));
    }

    match cost {
        Some(AbilityCost::Tap) => tap_source(state, source_id, events)?,
        // CR 605.3a + CR 601.2h: Top-level mana sub-cost (e.g. hypothetical
        // `{R}: Add {G}{G}`). Composite costs route through the Composite arm.
        Some(AbilityCost::Mana { cost }) => {
            match pay_mana_sub_cost(
                state,
                source_id,
                player,
                ability_index,
                cost,
                chosen_hybrid_payment,
                events,
                excluded_sources,
                sub_cost_demand,
                parent,
            )? {
                ManaAbilityCostComponentProgress::Complete => {}
                ManaAbilityCostComponentProgress::Paused {
                    remaining_life_payments,
                    choice_player,
                } => {
                    return Ok(ManaAbilityCostComponentProgress::Paused {
                        remaining_life_payments,
                        choice_player,
                    });
                }
            }
        }
        // CR 605.1a (2026 amendment): unreachable by construction — an activated
        // ability whose cost moves a card to or from a library is no longer a mana
        // ability, so no `Mill` cost reaches this payer. Retained rather than
        // deleted because the `match` over `AbilityCost` is exhaustive and this is
        // the shared mana-ability cost payer; deleting the arm would require
        // inventing an error path for a case the classifier already prevents. If
        // `is_mana_ability` is ever relaxed, this arm is already correct.
        // CR 701.17a: mill puts cards from the top of a library into a graveyard.
        Some(AbilityCost::Mill { count }) => mill_for_mana_cost(state, player, *count, events)?,
        Some(AbilityCost::PayLife { amount }) => {
            // CR 119.4 + CR 903.4: QuantityExpr resolves against the activator's
            // current state (e.g. commander color identity count).
            let resolved =
                super::quantity::resolve_quantity(state, amount, player, source_id).max(0) as u32;
            match pay_life_cost(state, player, resolved, events)? {
                ManaAbilityCostComponentProgress::Complete => {}
                ManaAbilityCostComponentProgress::Paused {
                    remaining_life_payments,
                    choice_player,
                } => {
                    return Ok(ManaAbilityCostComponentProgress::Paused {
                        remaining_life_payments,
                        choice_player,
                    });
                }
            }
        }
        Some(AbilityCost::TapCreatures {
            requirement,
            filter,
        }) => {
            // CR 605.1a: Mana-ability tap costs (Convoke-style) are fixed-count
            // only; the aggregate "total power N" form is reserved for
            // Crew/Saddle/Teamwork, which are never mana abilities.
            let count = requirement.fixed_count().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Aggregate-power tap cost is not valid for a mana ability".to_string(),
                )
            })?;
            for _ in 0..count {
                let chosen_id = chosen_tappers.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing tapped creature selection for mana ability".to_string(),
                    )
                })?;
                tap_selected_creature_for_mana_cost(
                    state,
                    source_id,
                    player,
                    chosen_id,
                    filter,
                    cost_has_source_tap_component(cost),
                    events,
                )?;
            }
        }
        Some(AbilityCost::Discard {
            count,
            filter,
            selection,
            self_scope,
        }) => {
            if selection.is_random() {
                return Err(EngineError::InvalidAction(
                    "Unsupported random discard cost for mana ability".to_string(),
                ));
            }
            if self_scope.is_source_card() {
                match crate::game::effects::discard::discard_as_cost(
                    state, source_id, player, events,
                ) {
                    crate::game::effects::discard::DiscardOutcome::Complete => {}
                    crate::game::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => {}
                }
            } else {
                let resolved = super::quantity::resolve_quantity(state, count, player, source_id)
                    .max(0) as usize;
                for _ in 0..resolved {
                    let chosen_id = chosen_discards.next().ok_or_else(|| {
                        EngineError::InvalidAction(
                            "Missing discarded card selection for mana ability".to_string(),
                        )
                    })?;
                    discard_selected_card_for_mana_cost(
                        state,
                        source_id,
                        player,
                        chosen_id,
                        filter.as_ref(),
                        events,
                    )?;
                }
            }
        }
        // CR 118.3 + CR 605.3b: Self-sacrifice mana ability costs are paid
        // atomically before mana production. This is the Treasure / Eldrazi
        // Spawn / Lotus Petal shape.
        Some(AbilityCost::Sacrifice(cost))
            if matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1) =>
        {
            if deferred_spell_sacrifice_reserved(state, source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "This permanent is already committed to a spell sacrifice cost".to_string(),
                ));
            }
            if super::static_abilities::player_cant_sacrifice_as_cost(state, player, source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot sacrifice this permanent as a cost".to_string(),
                ));
            }
            if matches!(
                sacrifice::sacrifice_permanent(state, source_id, player, events)?,
                sacrifice::SacrificeOutcome::NeedsReplacementChoice(_)
            ) {
                return Err(EngineError::InvalidAction(
                    "Mana ability sacrifice replacement pause must be owned by the activation cursor"
                        .to_string(),
                ));
            }
        }
        // CR 117.1 + CR 118.3 + CR 605.3b: Non-self sacrifice-from-battlefield
        // as a mana ability cost (Phyrexian Altar class). The interactive flow
        // has already captured the chosen permanents; verify each is still
        // legal and route through the sacrifice replacement pipeline.
        Some(AbilityCost::Sacrifice(cost))
            if !matches!(cost.target, TargetFilter::SelfRef)
                && matches!(
                    cost.requirement,
                    crate::types::ability::SacrificeRequirement::Count { .. }
                ) =>
        {
            let crate::types::ability::SacrificeRequirement::Count { count } = cost.requirement
            else {
                unreachable!("guarded above");
            };
            let target = &cost.target;
            for _ in 0..count {
                let chosen_id = chosen_sacrificed_battlefield.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing sacrificed permanent selection for mana ability".to_string(),
                    )
                })?;
                if matches!(
                    sacrifice_selected_permanent_for_mana_cost(
                        state, source_id, player, chosen_id, target, events,
                    )?,
                    sacrifice::SacrificeOutcome::NeedsReplacementChoice(_)
                ) {
                    return Err(EngineError::InvalidAction(
                        "Mana ability sacrifice replacement pause must be owned by the activation cursor"
                            .to_string(),
                    ));
                }
            }
        }
        Some(AbilityCost::Exile { .. }) => {
            return Err(EngineError::InvalidAction(
                "Mana ability exile costs must be paid by the activation cursor".to_string(),
            ));
        }
        // CR 605.2 + CR 701.59: Bare collect-evidence mana-ability cost. The
        // exile already happened interactively via the `CollectEvidenceChoice`
        // resume; no-op so the cost is neither re-paid nor errored.
        Some(AbilityCost::CollectEvidence { .. }) => {}
        // CR 107.3a/.3c + CR 702.179f: Bare `Pay X speed` mana-ability cost
        // (Chicago Loop). Concretize the announced X into a Fixed cost, then
        // delegate to the single-authority cost payer.
        Some(AbilityCost::PaySpeed { amount }) => {
            let cost = match chosen_x {
                Some(x) => AbilityCost::PaySpeed {
                    amount: QuantityExpr::Fixed { value: x as i32 },
                },
                None => AbilityCost::PaySpeed {
                    amount: amount.clone(),
                },
            };
            if matches!(
                super::costs::pay_ability_cost_for_activation(
                    state,
                    player,
                    source_id,
                    &cost,
                    ability_index,
                    events,
                )?,
                super::costs::PaymentOutcome::Paused { .. }
            ) {
                return Err(EngineError::InvalidAction(
                    "Mana ability replacement pause must be owned by the activation cursor"
                        .to_string(),
                ));
            }
        }
        // Self-contained components (Untap, Exert, PayEnergy, self-ReturnToHand,
        // EffectCost) delegate to the single-authority cost payer.
        Some(c) if is_self_contained_mana_subcost(c) => {
            if matches!(
                super::costs::pay_ability_cost_for_activation(
                    state,
                    player,
                    source_id,
                    c,
                    ability_index,
                    events,
                )?,
                super::costs::PaymentOutcome::Paused { .. }
            ) {
                return Err(EngineError::InvalidAction(
                    "Mana ability replacement pause must be owned by the activation cursor"
                        .to_string(),
                ));
            }
        }
        // CR 122.1 + CR 601.2b: Standalone RemoveCounter-on-self mana-ability
        // cost (Pentad Prism, Crystalline Crawler, Druids' Repository class).
        Some(AbilityCost::RemoveCounter {
            count,
            counter_type,
            target: None,
            ..
        }) => {
            let count = match *count {
                REMOVE_COUNTER_COST_ANY_NUMBER => chosen_counter_count.ok_or_else(|| {
                    EngineError::InvalidAction("Missing counter count for mana ability".to_string())
                })?,
                REMOVE_COUNTER_COST_ALL => {
                    removable_counter_count_for_mana_cost(state, source_id, counter_type)
                }
                count => count,
            };
            remove_counters_for_mana_cost(state, source_id, counter_type, count, events);
        }
        Some(other) => {
            return Err(EngineError::InvalidAction(format!(
                "Unsupported mana ability cost: {other:?}"
            )));
        }
        None => {}
    }

    Ok(ManaAbilityCostComponentProgress::Complete)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManaAbilityCostComponentProgress {
    Complete,
    Paused {
        remaining_life_payments: Vec<u32>,
        choice_player: Option<PlayerId>,
    },
}

fn cost_sacrifices_reserved_source(
    state: &GameState,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> bool {
    deferred_spell_sacrifice_reserved(state, source_id)
        && cost.as_ref().is_some_and(ability_cost_sacrifices_source)
}

fn ability_cost_sacrifices_source(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice(cost) => {
            matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1)
        }
        AbilityCost::Composite { costs } => costs.iter().any(ability_cost_sacrifices_source),
        _ => false,
    }
}

/// CR 605.1a + CR 701.17a-b: Pay a `Mill` cost component of a mana ability by
/// milling `count` cards from the activating player's library into their
/// graveyard. Routes through the replacement pipeline (mirroring `mill::resolve`
/// and the rad-counter handler) so graveyard-redirect replacements (Rest in
/// Peace / Leyline of the Void) apply and "a card was put into a graveyard"
/// triggers see the milled cards.
///
/// Millikin (`{T}, Mill a card: Add {C}`) **was** the canonical case and is no
/// longer a mana ability under CR 605.1a's 2026 library criterion, so this
/// function is unreachable from the mana fast path — see the
/// `Some(AbilityCost::Mill { .. })` arm of the mana-ability cost payer above,
/// which records why the arm is retained rather than deleted. The mill mechanics
/// below remain correct for a relaxed classifier or a future non-library mill
/// cost.
fn mill_for_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let proposed = crate::types::proposed_event::ProposedEvent::Mill {
        player_id: player,
        count,
        destination: Zone::Graveyard,
        applied: Default::default(),
    };
    match super::replacement::replace_event(state, proposed, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            // CR 616.1: a per-card `Moved` ordering choice parks the prompt
            // (`state.waiting_for` left set, tail in the active BatchDelivery frame);
            // bail like `mill::resolve` does so the surfaced prompt is not
            // clobbered. The parked activation resumes the remaining cost
            // components and mana production.
            if !super::effects::mill::apply_mill_after_replacement(state, event, events).map_err(
                |e| EngineError::InvalidAction(format!("Mill cost could not be paid: {e:?}")),
            )? {
                return Ok(());
            }
        }
        // CR 701.17b: "mill as many as you can" — a fully replaced-away or empty
        // library still pays the cost (milling zero cards is legal).
        super::replacement::ReplacementResult::Prevented => {}
        super::replacement::ReplacementResult::NeedsChoice(choosing_player) => {
            state.waiting_for =
                super::replacement::replacement_choice_waiting_for(choosing_player, state);
        }
    }
    Ok(())
}

/// Single-authority delegation gate: self-contained mana-ability cost components
/// (non-interactive, non-mana, non-tap, no `chosen_*` selection) that the
/// activated-ability cost payer (`super::costs::pay_ability_cost`) already
/// resolves correctly. Routing these through that one authority — rather than
/// duplicating each CR-annotated body here — keeps replacement routing and rule
/// annotations in a single place (CLAUDE.md: "single authority for ability
/// costs"). Covers Untap ({Q}, Pili-Pala), Exert (Arena of Glory / Oasis
/// Ritualist), PayEnergy (Aether Hub class), self-`ReturnToHand` (Grinning
/// Ignus), and `EffectCost` put-counter-on-self (Wall of Roots).
///
/// `PaySpeed` is deliberately excluded: Chicago Loop's `Pay X speed: Add X mana`
/// couples a player-announced X to both cost and effect (CR 601.2b), which the
/// non-announcing delegation path cannot express — it is handled on its own.
///
/// `CollectEvidence` is also deliberately excluded: Cryptex's `Collect evidence 3`
/// is paid interactively via the `CollectEvidenceChoice` prompt that
/// `advance_mana_ability_activation` surfaces before mana production (CR 701.59),
/// so it is a no-op in the cost-payment match rather than a delegated payment.
fn is_self_contained_mana_subcost(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Untap
        | AbilityCost::Exert
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::ReturnToHand {
            filter: Some(TargetFilter::SelfRef),
            ..
        } => true,
        // CR 122.1 + CR 601.2b: Pentad Prism / Everflowing Chalice — bare
        // self-RemoveCounter mana-ability costs (no tap) delegate to the
        // activated-ability cost payer. "Remove any number" stays on the
        // interactive mana-ability path in `advance_mana_ability_activation`.
        AbilityCost::RemoveCounter {
            target: None,
            count,
            ..
        } => !crate::types::ability::is_chosen_remove_counter_cost_count(*count),
        _ => false,
    }
}

fn pay_life_cost(
    state: &mut GameState,
    player: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<ManaAbilityCostComponentProgress, EngineError> {
    // CR 118.3 + CR 119.4 + CR 119.8: Delegate to the single-authority helper
    // so mana-ability life costs honor the replacement pipeline and the
    // CantLoseLife lock identically to every other pay-life path.
    match life_costs::pay_life_as_cast_or_activation_cost(state, player, amount, events) {
        PayLifeCostResult::Paid { .. } => Ok(ManaAbilityCostComponentProgress::Complete),
        PayLifeCostResult::PaidWithDeferredSubstitution { .. } => {
            Ok(ManaAbilityCostComponentProgress::Paused {
                remaining_life_payments: Vec::new(),
                choice_player: None,
            })
        }
        PayLifeCostResult::DeferredReplacementChoice { choice_player, .. } => {
            Ok(ManaAbilityCostComponentProgress::Paused {
                remaining_life_payments: Vec::new(),
                choice_player: Some(choice_player),
            })
        }
        PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => Err(
            EngineError::ActionNotAllowed("Cannot pay life cost for mana ability".to_string()),
        ),
    }
}

/// CR 605.3a + CR 605.1a: Extract the nested `ManaCost` from an ability cost
/// that contains a mana sub-cost (either at top level or inside a Composite).
/// Returns `None` for costs with no mana payment component.
pub(crate) fn mana_sub_cost_of(cost: &Option<AbilityCost>) -> Option<&ManaCost> {
    match cost {
        Some(AbilityCost::Mana { cost }) => Some(cost),
        Some(AbilityCost::Composite { costs }) => costs.iter().find_map(|c| match c {
            AbilityCost::Mana { cost } => Some(cost),
            _ => None,
        }),
        _ => None,
    }
}

/// CR 605.3a + CR 605.3b: True iff this cost resolves with NO player prompt once
/// the produced color is pre-chosen — i.e. it hits none of the five interactive
/// cost gates that `advance_mana_ability_activation` checks before producing
/// mana (discard, tap-creatures, non-self exile, non-self sacrifice, and the
/// mana sub-cost handled by `find_*`/`mana_sub_cost_of` directly above/below).
/// This is the eligibility gate for bulk activation: only such sources can be
/// batched behind a single shared color decision (CR 605.3b — no stack, resolves
/// immediately).
///
/// Deny-by-default whitelist — only `Tap`, self-sacrifice (`SelfRef`, the
/// Treasure/Gold cost shape), and `Composite`s built solely from those qualify.
/// Every other cost variant — including any added later — is treated as
/// choice-bearing and excluded, so a new interactive cost can never silently
/// become batchable. Kept beside the gate matchers so the whitelist stays in
/// lockstep if a sixth gate is introduced.
fn cost_resolves_without_choice(cost: &Option<AbilityCost>) -> bool {
    cost.as_ref().is_none_or(cost_component_choice_free)
}

/// CR 605.3a: True iff a single cost node resolves with no player prompt. The
/// full-tree building block behind [`cost_resolves_without_choice`]: a
/// `Composite` qualifies only when **every** component qualifies, so a
/// self-sacrifice component sitting beside a choice-bearing sibling (Lion's Eye
/// Diamond's `Discard`) is correctly rejected. Shared with
/// `mana_sources::has_unambiguous_self_sacrifice_component` so the auto-tap
/// eligibility gate applies the identical whole-tree invariant rather than a
/// per-component `any` match.
pub(crate) fn cost_component_choice_free(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap => true,
        AbilityCost::Sacrifice(cost)
            if matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1) =>
        {
            true
        }
        AbilityCost::Composite { costs } => costs.iter().all(cost_component_choice_free),
        _ => false,
    }
}

/// CR 605.3a: The controller's *other* permanents that could be activated for
/// the same `SingleColor` mana choice — identical ability definition, choice-
/// free cost, and currently activatable (untapped, on the battlefield, not
/// summoning-sick, via the shared `mana_ability_ready_without_simulation` gate).
/// These are the sources `GameAction::ChooseManaColor` may bulk-activate with the
/// chosen color. `exclude` is the just-activated source (already cost-paid, so
/// omitted). Sorted by id for deterministic ordering across the WASM/multiplayer
/// boundary.
fn batch_eligible_siblings(
    state: &GameState,
    player: PlayerId,
    exclude: ObjectId,
    ability_def: &AbilityDefinition,
) -> Vec<ObjectId> {
    // A permanent may carry the same ability definition more than once (granted by
    // multiple Auras/effects). Checking only the first matching index would wrongly
    // reject the source when that copy is unavailable (e.g. a once-each-turn
    // restriction) while a later identical copy is ready, so test whether *any*
    // matching ability index is ready.
    let mut siblings: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter_map(|id| {
            let obj = state.objects.get(&id)?;
            (id != exclude
                && obj.controller == player
                && obj.abilities.iter().enumerate().any(|(index, ability)| {
                    ability == ability_def
                        && mana_ability_ready_without_simulation(
                            state,
                            player,
                            id,
                            index,
                            ability_def,
                        )
                }))
            .then_some(id)
        })
        .collect();
    siblings.sort_unstable_by_key(|id| id.0);
    siblings
}

/// CR 107.4e + CR 601.2h: Enumerate legal per-hybrid-shard color assignments
/// for a mana-ability mana sub-cost. Each returned vector aligns 1:1 with
/// hybrid shards in `cost` in printed order. A plan is included iff a clone
/// of `pool` can be fully debited when each hybrid shard is pinned to the
/// chosen color.
///
/// For a cost with zero hybrid shards the result is `[vec![]]` when the pool
/// covers the cost (representing the trivial empty-choice plan), or empty
/// when the pool cannot cover. Callers short-circuit the single-plan case
/// into auto-pay.
fn enumerate_hybrid_payment_plans(
    pool: &ManaPool,
    cost: &ManaCost,
    ctx: &PaymentContext<'_>,
) -> Vec<Vec<ManaType>> {
    let hybrid_pairs = hybrid_shard_pairs(cost);
    let mut plans = Vec::new();
    enumerate_plans_rec(pool, cost, ctx, &hybrid_pairs, &mut Vec::new(), &mut plans);
    plans
}

/// List the (a, b) color pairs for each hybrid shard in printed order.
/// Only pure hybrid shards (`{W/U}` style) contribute — Phyrexian hybrid
/// shards resolve via the mana-payment life-fallback path and
/// colorless-hybrid (`{C/W}`) defers to the auto-pay preference, which
/// matches how casting treats them.
fn hybrid_shard_pairs(cost: &ManaCost) -> Vec<(ManaType, ManaType)> {
    let ManaCost::Cost { shards, .. } = cost else {
        return Vec::new();
    };
    shards
        .iter()
        .filter_map(|&shard| match mana_payment::shard_to_mana_type(shard) {
            mana_payment::ShardRequirement::Hybrid(a, b) => Some((a, b)),
            _ => None,
        })
        .collect()
}

fn enumerate_plans_rec(
    pool: &ManaPool,
    cost: &ManaCost,
    ctx: &PaymentContext<'_>,
    hybrid_pairs: &[(ManaType, ManaType)],
    chosen: &mut Vec<ManaType>,
    out: &mut Vec<Vec<ManaType>>,
) {
    if chosen.len() == hybrid_pairs.len() {
        if try_pay_with_hybrid_plan(pool, cost, chosen, ctx).is_some() {
            out.push(chosen.clone());
        }
        return;
    }
    let (a, b) = hybrid_pairs[chosen.len()];
    chosen.push(a);
    enumerate_plans_rec(pool, cost, ctx, hybrid_pairs, chosen, out);
    chosen.pop();
    if a != b {
        chosen.push(b);
        enumerate_plans_rec(pool, cost, ctx, hybrid_pairs, chosen, out);
        chosen.pop();
    }
}

/// CR 107.4e: Simulate paying `cost` from a clone of `pool` with hybrid
/// shards pinned to the colors in `plan`. Returns `Some(())` when the pool
/// covers the cost, `None` otherwise. Deterministic — uses the same
/// auto-pay rules as `pay_cost` except hybrid shards defer to `plan`.
fn try_pay_with_hybrid_plan(
    pool: &ManaPool,
    cost: &ManaCost,
    plan: &[ManaType],
    ctx: &PaymentContext<'_>,
) -> Option<()> {
    // CR 106.6: Plan publication and auto-selection must use the same
    // activation context as the authoritative real debit. Otherwise the
    // engine can offer a restricted mana unit that execution then rejects.
    // The simulated spent units are discarded; provenance is recorded only
    // at the real payment site.
    select_cost_with_plan(pool, cost, plan, Some(ctx))
        .ok()
        .map(|_| ())
}

/// CR 107.4e + CR 601.2h: Select the exact units that pay `cost` using `plan` for hybrid
/// shards. Non-hybrid shards (single, Phyrexian, snow, colorless-hybrid,
/// hybrid-Phyrexian, two-generic-hybrid, X) are routed through the same
/// auto-pay rules the casting flow uses via `mana_payment::pay_from_pool`, but
/// with the hybrid shards already resolved, the plan is unambiguous.
///
/// Implementation: build a scratch cost with hybrid shards rewritten to
/// single-color shards per `plan`, then delegate to the shared selector. This keeps
/// every shard-kind's payment rules in one place.
fn select_cost_with_plan(
    pool: &ManaPool,
    cost: &ManaCost,
    plan: &[ManaType],
    ctx: Option<&PaymentContext<'_>>,
) -> Result<Vec<crate::types::mana::ManaUnit>, mana_payment::PaymentError> {
    use crate::types::mana::ManaCostShard;
    let ManaCost::Cost { shards, generic } = cost else {
        return Ok(Vec::new());
    };
    let mut plan_cursor = 0usize;
    let rewritten_shards: Vec<ManaCostShard> = shards
        .iter()
        .map(|&shard| match mana_payment::shard_to_mana_type(shard) {
            mana_payment::ShardRequirement::Hybrid(..) => {
                let color = plan[plan_cursor];
                plan_cursor += 1;
                mana_type_to_single_shard(color)
            }
            _ => shard,
        })
        .collect();
    let scratch_cost = ManaCost::Cost {
        shards: rewritten_shards,
        generic: *generic,
    };
    // CR 106.6: Route through the restriction-aware payment path so the
    // player's context (activation or spell) gates eligible mana units.
    // CR 107.4f: Mana-ability sub-cost payment doesn't surface a player-side
    // ShardChoice and is paid implicitly during ability resolution; pass an
    // empty `LifePaymentColors` since K'rrik substitution does not apply to
    // mana abilities' own activation costs in any printed exemplar today.
    mana_payment::select_mana_payment(
        pool,
        &scratch_cost,
        None,
        ctx,
        false,
        None,
        crate::types::mana::LifePaymentColors::EMPTY,
        // CR 118.3a: mana-ability activation sub-costs are not pinnable.
        &[],
    )
    .map(|(spent, _life)| spent)
}

/// Map a `ManaType` to the printed-shard variant that requires exactly that
/// color (used to pin hybrid shards after the player's color choice).
fn mana_type_to_single_shard(color: ManaType) -> crate::types::mana::ManaCostShard {
    use crate::types::mana::ManaCostShard;
    match color {
        ManaType::White => ManaCostShard::White,
        ManaType::Blue => ManaCostShard::Blue,
        ManaType::Black => ManaCostShard::Black,
        ManaType::Red => ManaCostShard::Red,
        ManaType::Green => ManaCostShard::Green,
        ManaType::Colorless => ManaCostShard::Colorless,
    }
}

/// CR 605.3a + CR 602.2b + CR 601.2g-h: Pay a mana sub-cost for an activated
/// mana ability. If `hybrid_plan` is provided, hybrid shards are pinned to the
/// colors chosen by `PayManaAbilityMana` and debited from the current pool.
/// Otherwise, use the shared activation mana-payment building block so the
/// player may activate other mana abilities while paying this activation cost.
#[allow(clippy::too_many_arguments)]
fn pay_mana_sub_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_index: Option<usize>,
    cost: &ManaCost,
    hybrid_plan: Option<&[ManaType]>,
    events: &mut Vec<GameEvent>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    parent: Option<&ManaAbilityCostParent>,
) -> Result<ManaAbilityCostComponentProgress, EngineError> {
    let Some(hybrid_plan) = hybrid_plan else {
        // CR 605.3c: Every source already in `excluded_sources` is an ancestor
        // mana-ability activation that is synchronously suspended on the call
        // stack mid-payment (its cost is still being paid; it has not yet
        // resolved). CR 605.3c ("Once a player begins to activate a mana
        // ability, that ability can't be activated again until it has
        // resolved") applies to each ancestor link individually, so the entire
        // in-flight chain must be excluded from auto-tap — not just `source_id`.
        // Extending the chain here (rather than rebuilding it from
        // `{source_id}`) is what makes a 2-source cross-payment, an N-source
        // chain, or a self-loop terminate instead of recursing infinitely.
        let mut excluded_sources = excluded_sources.clone();
        excluded_sources.insert(source_id);
        let payment = super::casting::pay_ability_mana_cost_excluding_with_parent(
            state,
            player,
            source_id,
            ability_index,
            cost,
            events,
            &excluded_sources,
            sub_cost_demand,
            parent,
        )?;
        let choice_player = state
            .pending_replacement
            .is_some()
            .then(|| state.waiting_for.acting_player())
            .flatten();
        return Ok(match payment {
            super::casting::ManaCostPayment::Paid(()) => ManaAbilityCostComponentProgress::Complete,
            super::casting::ManaCostPayment::Paused {
                remaining_life_payments,
                ..
            } => ManaAbilityCostComponentProgress::Paused {
                remaining_life_payments,
                choice_player,
            },
        });
    };

    // CR 106.6: The mana sub-cost of a mana ability is paid as part of an
    // ability activation — spend-restrictions must be evaluated through
    // `allows_activation` (via `PaymentContext::Activation`), not through the
    // pool's restriction-blind `pay_cost`. Without this, activation-only
    // mana (e.g. Heart of Ramos) would silently pay through for the {R} half
    // of a hypothetical "{R}: Add {G}{G}" mana ability.
    let activation_context =
        super::casting::activation_payment_context(state, source_id, ability_index);
    let ctx = activation_context.as_payment_context();
    state.restamp_pool_pip_ids(player);
    let spent = select_cost_with_plan(
        &state.players[player.0 as usize].mana_pool,
        cost,
        hybrid_plan,
        Some(&ctx),
    )
    .map_err(|_| {
        EngineError::ActionNotAllowed("Mana pool cannot cover mana ability cost".to_string())
    })?;
    let recipient = state.mana_payment_recipient(source_id, player);
    state
        .resolve_and_apply_mana_spend(player, recipient, &spent)
        .map_err(|_| {
            EngineError::ActionNotAllowed("Mana pool changed before payment applied".to_string())
        })?;
    state.layers_dirty.mark_full();
    // CR 605.3b: The player's mana pool mutation is the public signal; no
    // dedicated event exists for ability mana payments. The pool-diff is
    // surfaced via the standard state-update machinery.
    let _ = events;
    Ok(ManaAbilityCostComponentProgress::Complete)
}

/// CR 605.3b: Complete a `PayManaAbilityMana` prompt by validating the
/// submitted payment against the enumerated options and resuming activation.
pub fn handle_pay_mana_ability_mana(
    state: &mut GameState,
    options: &[Vec<ManaType>],
    pending: &PendingManaAbility,
    payment: &[ManaType],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !options.iter().any(|opt| opt.as_slice() == payment) {
        return Err(EngineError::InvalidAction(
            "Chosen mana payment is not among the legal options".to_string(),
        ));
    }
    let mut updated = pending.clone();
    updated.chosen_mana_payment = Some(payment.to_vec());
    advance_mana_ability_activation(state, updated, events)
}

fn any_number_self_remove_counter_cost(cost: &Option<AbilityCost>) -> Option<&CounterMatch> {
    match cost.as_ref()? {
        AbilityCost::RemoveCounter {
            count: REMOVE_COUNTER_COST_ANY_NUMBER,
            counter_type,
            target: None,
            ..
        } => Some(counter_type),
        AbilityCost::Composite { costs } => costs.iter().find_map(|cost| match cost {
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_ANY_NUMBER,
                counter_type,
                target: None,
                ..
            } => Some(counter_type),
            _ => None,
        }),
        _ => None,
    }
}

fn removable_counter_count_for_mana_cost(
    state: &GameState,
    source_id: ObjectId,
    counter_type: &CounterMatch,
) -> u32 {
    let Some(obj) = state.objects.get(&source_id) else {
        return 0;
    };
    match counter_type {
        CounterMatch::Any => obj.counters.values().copied().sum(),
        CounterMatch::OfType(counter_type) => obj.counters.get(counter_type).copied().unwrap_or(0),
    }
}

fn remove_counters_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    counter_type: &CounterMatch,
    mut count: u32,
    events: &mut Vec<GameEvent>,
) {
    match counter_type {
        CounterMatch::OfType(counter_type) => {
            super::effects::counters::remove_counter_with_replacement(
                state,
                source_id,
                counter_type.clone(),
                count,
                events,
            );
        }
        CounterMatch::Any => {
            let counters: Vec<(CounterType, u32)> = state
                .objects
                .get(&source_id)
                .map(|obj| {
                    obj.counters
                        .iter()
                        .map(|(counter_type, count)| (counter_type.clone(), *count))
                        .collect()
                })
                .unwrap_or_default();
            for (counter_type, available) in counters {
                if count == 0 {
                    break;
                }
                let to_remove = available.min(count);
                if to_remove > 0 {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        counter_type,
                        to_remove,
                        events,
                    );
                    count -= to_remove;
                }
            }
        }
    }
}

/// Tap a permanent as part of paying a mana ability cost.
fn tap_source(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.tapped {
        return Err(EngineError::ActionNotAllowed(
            "Cannot activate tap ability: permanent is tapped".to_string(),
        ));
    }
    // CR 701.26a + CR 508.1f: route the {T} mana-ability tap through the single
    // authority so a "can't become tapped" source is refused.
    crate::game::restrictions::tap_permanent_for_cost(state, source_id, events)?;
    Ok(())
}

fn tap_creature_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (requirement, filter) = super::casting::find_tap_creatures_cost(cost.as_ref()?)?;
    // CR 605.1a: mana-ability tap costs (Convoke-style) are fixed-count only.
    let count = requirement.fixed_count()?;
    let creatures = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if cost_has_source_tap_component(cost) && id == source_id {
                return false;
            }
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.zone != Zone::Battlefield || obj.controller != player || obj.tapped {
                return false;
            }
            matches_target_filter(
                state,
                id,
                filter,
                &FilterContext::from_source(state, source_id),
            )
        })
        .collect();
    Some((count as usize, creatures))
}

fn discard_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let cost = cost.as_ref()?;
    // Mana-ability interactive discard applies only to a player-CHOSEN discard leg; a
    // non-Chosen discard (e.g. random / top-of-hand) is not a mid-activation card selection,
    // so this interactive surfacing does not handle it. (Pre-existing scope; keeps blast
    // radius nil.) `find_non_self_discard` is the single detector shared with the
    // casting/activation path; the `Chosen` gate below is the ONLY mana-specific divergence.
    let (_count, _filter, selection) = super::casting::find_non_self_discard(cost)?;
    if selection != CardSelectionMode::Chosen {
        return None;
    }
    // Single authority for the zero-count auto-pay + payability rules (CR 601.2h + CR 701.9a):
    // delegate to `resolve_non_self_discard_requirement` so the mana path stays aligned with the
    // activation path in one place. `Ok(Some)` => interactive selection; `Ok(None)` => zero-card
    // discard paid by doing nothing (skip the leg). `Err` (fewer eligible cards than the nonzero
    // count) is unreachable here because `cost_payability` already gated activation on hand size,
    // so `unwrap_or_default()`'s `None` fallback is the correct "no selection to surface" result.
    super::casting::resolve_non_self_discard_requirement(state, player, source_id, cost)
        .unwrap_or_default()
}

/// CR 117.1 + CR 118.3: Match non-self `AbilityCost::Exile` shapes. Returns
/// `(count, effective_zone, filter)` if found, else `None`.
fn find_exile_cost(cost: &AbilityCost) -> Option<(u32, Zone, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } if !matches!(filter, Some(TargetFilter::SelfRef)) => Some((
            *count,
            exile_cost_effective_zone(*zone, filter.as_ref()),
            filter.as_ref(),
        )),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_exile_cost),
        _ => None,
    }
}

/// CR 117.1 + CR 118.3 + CR 605.3b: Surface eligible objects for a non-self
/// exile mana ability cost. Library costs are deterministic top-card payment,
/// not a player choice, so they are prepared separately.
fn exile_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Zone, Vec<ObjectId>)> {
    let (count, zone, filter) = find_exile_cost(cost.as_ref()?)?;
    if zone == Zone::Library {
        return None;
    }
    let cards = eligible_exile_cost_objects(state, player, source_id, zone, filter, count)
        .into_iter()
        .filter(|id| !deferred_spell_sacrifice_reserved(state, *id))
        .collect();
    Some((count as usize, zone, cards))
}

fn prepare_deterministic_exile_cost_selection(
    state: &GameState,
    pending: &PendingManaAbility,
    cost: &Option<AbilityCost>,
) -> Result<Option<PendingManaAbility>, EngineError> {
    let Some((count, Zone::Library, filter)) = cost.as_ref().and_then(find_exile_cost) else {
        return Ok(None);
    };
    if count == 0 {
        return Ok(None);
    }
    if filter.is_some() {
        return Err(EngineError::InvalidAction(
            "Unsupported filtered library exile cost for mana ability".to_string(),
        ));
    }
    let chosen = eligible_exile_cost_objects(
        state,
        pending.player,
        pending.source_id,
        Zone::Library,
        None,
        count,
    );
    if chosen.len() < count as usize {
        return Err(EngineError::ActionNotAllowed(
            "Not enough cards in library to exile for mana ability cost".to_string(),
        ));
    }
    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });
    let mut updated = pending.clone();
    updated.chosen_exiled = chosen;
    updated.cost_paid_object = captured;
    Ok(Some(updated))
}

/// CR 117.1 + CR 118.3 + CR 605.3b: Surface eligible battlefield permanents
/// for an `AbilityCost::Sacrifice(SacrificeCost::count(!SelfRef, 1))` mana ability cost.
/// Delegates eligibility to the casting cost helper so mana and non-mana
/// activation costs share the same battlefield/controller/filter semantics.
fn sacrifice_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (count, filter) = super::casting::find_non_self_sacrifice_cost(cost.as_ref()?)?;
    let permanents =
        super::casting::find_eligible_sacrifice_targets(state, player, source_id, filter);
    Some((count as usize, permanents))
}

fn tap_selected_creature_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: &TargetFilter,
    exclude_source: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if exclude_source && chosen_id == source_id {
        return Err(EngineError::ActionNotAllowed(
            "Source cannot satisfy both tap costs".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&chosen_id)
        .ok_or_else(|| EngineError::InvalidAction("Selected creature not found".to_string()))?;
    if obj.zone != Zone::Battlefield || obj.controller != player || obj.tapped {
        return Err(EngineError::ActionNotAllowed(
            "Selected creature is not an untapped creature you control".to_string(),
        ));
    }
    if !matches_target_filter(
        state,
        chosen_id,
        filter,
        &FilterContext::from_source(state, source_id),
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Selected creature does not satisfy mana ability cost".to_string(),
        ));
    }

    // CR 701.26a + CR 508.1f: route the tap-another-creature mana cost through the
    // single authority so a "can't become tapped" creature is refused.
    crate::game::restrictions::tap_permanent_for_cost(state, chosen_id, events)?;
    Ok(())
}

fn discard_selected_card_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: Option<&TargetFilter>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let player_state = state
        .players
        .get(player.0 as usize)
        .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;
    if !player_state.hand.contains(&chosen_id) || chosen_id == source_id {
        return Err(EngineError::ActionNotAllowed(
            "Selected card is not eligible to discard for mana ability".to_string(),
        ));
    }
    if let Some(target_filter) = filter {
        if !matches_target_filter(
            state,
            chosen_id,
            target_filter,
            &FilterContext::from_source(state, source_id),
        ) {
            return Err(EngineError::ActionNotAllowed(
                "Selected card does not satisfy mana ability discard cost".to_string(),
            ));
        }
    }
    match crate::game::effects::discard::discard_as_cost(state, chosen_id, player, events) {
        crate::game::effects::discard::DiscardOutcome::Complete => Ok(()),
        crate::game::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => Ok(()),
    }
}

fn sacrifice_selected_permanent_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<sacrifice::SacrificeOutcome, EngineError> {
    let obj = state.objects.get(&chosen_id).ok_or_else(|| {
        EngineError::InvalidAction("Selected permanent for sacrifice cost not found".to_string())
    })?;
    if obj.zone != Zone::Battlefield || obj.controller != player {
        return Err(EngineError::ActionNotAllowed(
            "Selected permanent is not on the battlefield under your control".to_string(),
        ));
    }
    if !matches_target_filter(
        state,
        chosen_id,
        filter,
        &FilterContext::from_source(state, source_id),
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Selected permanent does not match the sacrifice cost filter".to_string(),
        ));
    }
    if super::static_abilities::player_cant_sacrifice_as_cost(state, player, chosen_id) {
        return Err(EngineError::ActionNotAllowed(
            "Selected permanent cannot be sacrificed as a cost".to_string(),
        ));
    }
    sacrifice::sacrifice_permanent(state, chosen_id, player, events)
}

fn cost_has_source_tap_component(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Tap) => true,
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|cost| matches!(cost, AbilityCost::Tap))
        }
        _ => false,
    }
}

pub(crate) fn resume_waiting_for(
    mana_source_controller: PlayerId,
    resume: ManaAbilityResume,
) -> WaitingFor {
    match resume {
        ManaAbilityResume::Priority => WaitingFor::Priority {
            player: mana_source_controller,
        },
        ManaAbilityResume::ManaPayment {
            outer_player,
            convoke_mode,
        } => WaitingFor::ManaPayment {
            player: outer_player.unwrap_or(mana_source_controller),
            convoke_mode,
        },
        ManaAbilityResume::ManaSourceSelection {
            player,
            options,
            convoke_mode,
        } => WaitingFor::ManaSourceSelection {
            player,
            options,
            convoke_mode,
        },
        ManaAbilityResume::UnlessPayment {
            outer_player,
            cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        } => WaitingFor::UnlessPayment {
            player: outer_player.unwrap_or(mana_source_controller),
            cost: *cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        },
        ManaAbilityResume::EffectPayCost { .. }
        | ManaAbilityResume::PhyrexianCastPayment { .. }
        | ManaAbilityResume::FinalizePendingManaPayment { .. }
        | ManaAbilityResume::CompanionToHand { .. }
        // CR 116.2c + CR 116.2b: like `CompanionToHand`, the pay-to-end and
        // turn-face-up special actions are resumed by
        // `resume_mana_ability_root`'s named arms, never here.
        | ManaAbilityResume::EndContinuousEffect { .. }
        | ManaAbilityResume::TurnFaceUp { .. } => {
            unreachable!("effect-cost resume is handled by resume_mana_ability_root")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;

    use crate::game::test_fixtures::mana_fixture_roles;

    /// **CR 605.4a — the acceptance decision is THREADED, not re-derived.**
    ///
    /// `is_triggered_mana_ability` answers CR 605.1b about a
    /// *classification-time* graph, and it is deliberately raw: a target
    /// anywhere makes it false. `is_resolving_triggered_mana` answers a
    /// different question at a different time — "is the occurrence currently
    /// executing an accepted triggered mana ability?" — because
    /// `resolve_ability_chain` may materialize an engine resolution-context
    /// referent into the overloaded `targets` vector partway through the very
    /// resolution whose status is being asked about. CR 605.4a says that
    /// occurrence stays stackless; re-asking the raw predicate would say
    /// otherwise and hand the body to the ordinary prompt path.
    ///
    /// Rows:
    ///
    /// * **(a) no marker, qualifying graph** ⇒ true, delegated;
    /// * **(b) no marker, targeted graph** ⇒ false, delegated. (a)+(b) are the
    ///   two-sided reach guard that the delegation is real rather than a
    ///   constant;
    /// * **(c) no marker, wrong firing event** ⇒ false, so the ambient
    ///   `current_trigger_event` is genuinely consulted;
    /// * **(d) marker live, targeted graph** ⇒ **true** — the delta. This is
    ///   exactly the clone shape (b) rejects, so the two rows differ only by
    ///   the marker;
    /// * **(e) marker cleared again** ⇒ (b)'s answer returns, proving the
    ///   marker is scope-shaped rather than sticky.
    ///
    /// REVERT-PROBE: delete the marker short-circuit so the helper always
    /// delegates ⇒ (d) flips to false while (a), (b), (c) and (e) still pass.
    /// The inverse probe — returning `true` whenever the marker is `None` — is
    /// caught by (b) and (c).
    #[test]
    fn the_accepted_occurrence_marker_is_the_resolution_time_classification_authority() {
        use crate::types::ability::{ManaProduction, QuantityExpr};
        use crate::types::resolved_commands::{RulesExecutionNodeRef, SettlementNodeOrdinal};

        let mana_effect = || Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };
        let untargeted = ResolvedAbility::new(mana_effect(), vec![], ObjectId(1), PlayerId(0));
        // The post-injection clone: identical body, but the resolver has
        // written a context referent into the overloaded `targets` vector.
        let injected = ResolvedAbility::new(
            mana_effect(),
            vec![crate::types::ability::TargetRef::Player(PlayerId(1))],
            ObjectId(1),
            PlayerId(0),
        );

        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: ManaType::Colorless,
            source_id: ObjectId(1),
            tap_state: ManaTapState::default(),
        });

        // (a) + (b): both truth values are reachable through delegation.
        assert!(
            is_resolving_triggered_mana(&state, &untargeted),
            "(a) with no accepted occurrence live this must BE the raw classifier"
        );
        assert!(
            !is_resolving_triggered_mana(&state, &injected),
            "(b) CR 605.1b at classification time: a target in the graph rejects"
        );

        // (c) the ambient firing event is genuinely part of the delegation.
        let restore_event = state.current_trigger_event.take();
        assert!(
            !is_resolving_triggered_mana(&state, &untargeted),
            "(c) CR 605.1b(b): no qualifying firing event, no triggered mana ability"
        );
        state.current_trigger_event = restore_event;

        // (d) THE DELTA: inside an accepted occurrence the same rejected clone
        // is still the accepted occurrence's own body.
        let node = RulesExecutionNodeRef::TriggeredMana(SettlementNodeOrdinal(1));
        state.active_rules_execution_node = Some(node);
        state.active_accepted_triggered_mana_node = Some(node);
        assert!(
            is_resolving_triggered_mana(&state, &injected),
            "(d) CR 605.4a: a resolution-context referent injected AFTER acceptance \
             does not make an already-accepted occurrence begin using the stack"
        );

        // (e) the marker is scope-shaped: restoring it restores (b)'s answer.
        state.active_accepted_triggered_mana_node = None;
        state.active_rules_execution_node = None;
        assert!(
            !is_resolving_triggered_mana(&state, &injected),
            "(e) outside the occurrence the raw classification-time answer returns"
        );
    }

    /// Matrix rows 15c + 20 — CR 605.1a classification is unchanged. This reader
    /// also bypasses `Effect::target_filter()`.
    ///
    /// CR 605.1a: a mana ability "doesn't require a target." Any declared role —
    /// recipient OR count source, context-ref or not — means the ability names a
    /// target and must use the stack. Writing this with `surfaced_filters()`
    /// would wrongly PROMOTE the ten context-ref cards to mana abilities, letting
    /// them resolve without the stack. The `target: None` positive is the reach
    /// guard: a blanket `return false` would satisfy every negative below.
    #[test]
    fn is_mana_ability_classification_unchanged_for_every_fixture_role() {
        use crate::types::ability::{ManaProduction, QuantityExpr};

        let mk = |target| {
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target,
                },
            )
        };

        for (name, role) in mana_fixture_roles() {
            assert!(
                !is_mana_ability(&mk(Some(role))),
                "{name}: a declared mana role means the ability targets (CR 605.1a)                  and must use the stack"
            );
        }

        // Jeska's Will shape: a COUNT-SOURCE-only role is still a target.
        assert!(
            !is_mana_ability(&mk(Some(
                crate::types::ability::ManaTargetRole::CountSource {
                    count_source: crate::types::ability::TargetFilter::Player,
                }
            ))),
            "a count-source-only mana ability targets and is not a mana ability"
        );

        // Reach guard / positive: an unqualified mana ability IS one.
        assert!(
            is_mana_ability(&mk(None)),
            "an unqualified mana ability (Cabal Coffers class) is still a mana ability"
        );
    }

    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityKind, AbilityTag, ActivationRestriction, Comparator,
        ContinuousModification, ControllerRef, CopyRetargetPermission, DelayedTriggerCondition,
        DevotionColors, Duration, Effect, EffectKind, FilterProp, LinkedExileScope,
        ManaContribution, ManaProduction, MultiTargetSpec, ObjectScope, PlayerFilter, PlayerScope,
        QuantityExpr, QuantityRef, SacrificeCost, StaticDefinition, TargetFilter,
        TriggerDefinition, TypeFilter, TypedFilter, REMOVE_COUNTER_COST_ANY_NUMBER,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{ExileLink, ExileLinkKind, PendingReplacement};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{
        ManaColor, ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit,
    };
    use crate::types::proposed_event::{ProposedEvent, ReplacementId};
    use crate::types::statics::{CostPaymentProhibition, ProhibitionScope, StaticMode};
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    #[test]
    fn post_delivery_mana_cost_pause_preserves_its_live_prompt() {
        let mut state = GameState::new_two_player(42);
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: vec![ReplacementId {
                source: ObjectId(7),
                index: 0,
            }],
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        let live_prompt = WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 1,
            cards: Vec::new(),
            source_id: ObjectId(7),
            effect_kind: EffectKind::Discard,
            up_to: false,
            unless_filter: None,
            discard_frame: None,
        };
        state.waiting_for = live_prompt.clone();

        pause_pre_delivery_mana_cost_replacement_choice(&mut state, None);
        assert_eq!(
            state.waiting_for, live_prompt,
            "a post-delivery substitute has no ordering player and retains its live prompt"
        );

        pause_pre_delivery_mana_cost_replacement_choice(&mut state, Some(PlayerId(0)));
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    player: PlayerId(0),
                    candidate_count: 1,
                    ..
                }
            ),
            "an explicit pre-delivery ordering player still opens the replacement choice"
        );
    }

    #[test]
    fn targetless_mana_reflexive_produces_mana_now_and_waits_on_stack() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        let source = create_object(
            &mut state,
            CardId(9901),
            player,
            "Rubble Rouser fixture".to_string(),
            Zone::Battlefield,
        );
        let mut reflexive = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
        );
        reflexive.condition = Some(AbilityCondition::WhenYouDo);
        reflexive.player_scope = Some(PlayerFilter::Opponent);
        let mana = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        })
        .sub_ability(reflexive);
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(mana);
        let opponent_life = state.players[1].life;

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .expect("activate targetless reflexive mana ability");

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[1].life, opponent_life);
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(
            state.stack[0].kind,
            crate::types::game_state::StackEntryKind::TriggeredAbility { .. }
        ));
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));

        let mut safety = 4;
        while !state.stack.is_empty() && safety > 0 {
            crate::game::engine::apply_as_current(
                &mut state,
                crate::types::actions::GameAction::PassPriority,
            )
            .expect("pass priority to resolve reflexive");
            safety -= 1;
        }
        assert_eq!(state.players[1].life, opponent_life - 2);
    }

    #[test]
    fn scoped_mana_ability_tap_event_aggregates_recipient_dependent_production() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(9900),
            PlayerId(0),
            "Opponent Mana Source".to_string(),
            Zone::Battlefield,
        );
        for (index, (controller, color)) in [
            (PlayerId(0), ManaColor::Green),
            (PlayerId(1), ManaColor::Blue),
            (PlayerId(2), ManaColor::Red),
        ]
        .into_iter()
        .enumerate()
        {
            let land = create_object(
                &mut state,
                CardId(9901 + index as u64),
                controller,
                format!("{color:?} Land"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
            Arc::make_mut(&mut state.objects.get_mut(&land).unwrap().abilities).push(
                make_mana_ability(ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                }),
            );
        }
        let ability = make_mana_ability(ManaProduction::OpponentLandColors {
            count: QuantityExpr::Fixed { value: 1 },
        })
        .player_scope(PlayerFilter::Opponent);
        let mut events = Vec::new();

        resolve_mana_ability(&mut state, source, PlayerId(0), &ability, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
        assert_eq!(state.players[1].mana_pool.total(), 1);
        assert_eq!(state.players[2].mana_pool.total(), 1);
        let recipient_colors = [
            state.players[1].mana_pool.mana[0].color,
            state.players[2].mana_pool.mana[0].color,
        ];
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::TappedForMana {
                player_id: PlayerId(0),
                source_id,
                produced,
                ..
            } if *source_id == source
                && *produced == recipient_colors
        )));
        let mut recipient_events: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ManaAbilityProduced {
                    player_id,
                    source_id,
                    produced,
                    ..
                } if *source_id == source => Some((*player_id, produced.clone())),
                _ => None,
            })
            .collect();
        recipient_events.sort_by_key(|(player, _)| *player);
        assert_eq!(
            recipient_events,
            vec![
                (PlayerId(1), vec![recipient_colors[0]]),
                (PlayerId(2), vec![recipient_colors[1]]),
            ],
            "each recipient receives one distinct aggregate ManaAbilityProduced event"
        );
    }

    fn gemstone_caverns_mana_ability() -> AbilityDefinition {
        let replacement = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: ManaColor::ALL.to_vec(),
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(CounterType::Generic("luck".to_string())),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }),
        });

        let mut ability = make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        });
        ability.sub_ability = Some(Box::new(replacement));
        ability
    }

    use crate::game::test_fixtures::brushland_colored_ability;

    fn seed_pool_with(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        for _ in 0..count {
            state.players[player.0 as usize].mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    fn seed_pool_with_restriction(
        state: &mut GameState,
        player: PlayerId,
        color: ManaType,
        restriction: ManaRestriction,
    ) {
        let _ = state.add_mana_to_pool(
            player,
            ManaUnit::new(color, ObjectId(0), false, vec![restriction]),
        );
    }

    fn expect_mana_ability_context(context: ManaChoiceContext) -> Box<PendingManaAbility> {
        match context {
            ManaChoiceContext::ManaAbility(pending) => pending,
            other => panic!("expected mana ability context, got {other:?}"),
        }
    }

    /// Skirk Prospector: "Sacrifice a Goblin: Add {R}". The sacrifice target is a
    /// *type* filter, not `SelfRef`, so the source survives and stays renewable.
    fn skirk_prospector_mana_ability() -> AbilityDefinition {
        make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Red],
            contribution: ManaContribution::Base,
        })
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Goblin".to_string()))),
            1,
        )))
    }

    // ───────────────────────────────────────────────────────────────────────
    // CR 605.1a (2026 amendment) — the library-movement criterion.
    //
    // "An activated ability is a mana ability if ... its cost and effect don't
    // move any card to or from a library."
    //
    // Rows V1-V13 of the plan's verification matrix. Every negative below is
    // paired with a positive reach-guard in the SAME test, built from the SAME
    // builder with the minimal one-node delta, so a fixture that never reaches
    // the seam cannot pass vacuously.
    // ───────────────────────────────────────────────────────────────────────

    /// `{T}: Add {C}` — the minimal mana ability every row below perturbs.
    fn colorless_tap_mana_ability() -> AbilityDefinition {
        make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        })
    }

    /// `{T}: Add {C}` with the root activation cost replaced (CR 602.1a).
    fn mana_ability_with_cost(cost: AbilityCost) -> AbilityDefinition {
        colorless_tap_mana_ability().cost(cost)
    }

    /// A bare chain link carrying `effect` and no cost.
    fn link(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, effect)
    }

    /// `{T}: Add {C}` with `effect` chained as the `sub_ability` — an
    /// instruction this ability follows during its own resolution (CR 608.2c).
    fn mana_ability_with_sub_effect(effect: Effect) -> AbilityDefinition {
        let mut def = colorless_tap_mana_ability();
        def.sub_ability = Some(Box::new(link(effect)));
        def
    }

    /// `{T}: Add {C}` with a fully-specified chain link.
    fn mana_ability_with_sub(sub: AbilityDefinition) -> AbilityDefinition {
        let mut def = colorless_tap_mana_ability();
        def.sub_ability = Some(Box::new(sub));
        def
    }

    fn draw_one() -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    fn surveil_one() -> Effect {
        Effect::Surveil {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    fn scry_one() -> Effect {
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    fn exile_cost(zone: Option<Zone>) -> AbilityCost {
        AbilityCost::Exile {
            count: 1,
            zone,
            filter: None,
        }
    }

    fn pay_life_one() -> AbilityCost {
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        }
    }

    /// A `CreateDelayedTrigger` wrapping `effect` — CR 603.7a, a separate
    /// ability that resolves later (CR 603.3).
    fn delayed(effect: Effect) -> Effect {
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep,
            },
            effect: Box::new(link(effect)),
            uses_tracked_set: false,
        }
    }

    /// V1 — CR 605.1a + CR 701.17a: a **root** cost-side `Mill` disqualifies.
    /// Millikin / Deranged Assistant: `{T}, Mill a card: Add {C}`.
    #[test]
    fn mill_cost_is_not_a_mana_ability() {
        let millikin = mana_ability_with_cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Mill { count: 1 }],
        });
        assert!(
            !is_mana_ability(&millikin),
            "CR 605.1a: a Mill cost moves a card from a library"
        );

        // Reach-guard, same builder, one-node delta: swap Mill for PayLife.
        let paid = mana_ability_with_cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, pay_life_one()],
        });
        assert!(
            is_mana_ability(&paid),
            "the identical shape with a non-library cost IS a mana ability"
        );
    }

    /// V2 — cost recursion reaches `Composite`, `OneOf`, and `PerCounter.base`.
    /// The last two are exactly what `mana_sources::cost_has_component` cannot
    /// see, which is why this criterion has its own recursive predicate.
    #[test]
    fn nested_cost_shapes_reach_the_library_predicate() {
        let composite = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Tap,
                AbilityCost::Mill { count: 1 },
            ],
        };
        let one_of = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::OneOf {
                    costs: vec![
                        AbilityCost::Mill { count: 1 },
                        AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                    ],
                },
            ],
        };
        let per_counter = AbilityCost::PerCounter {
            counter: CounterType::Generic("charge".to_string()),
            target: TargetFilter::SelfRef,
            base: Box::new(AbilityCost::Mill { count: 1 }),
        };
        for (label, cost) in [
            ("Composite", composite),
            ("OneOf nested in Composite", one_of),
            ("PerCounter base", per_counter),
        ] {
            assert!(
                !is_mana_ability(&mana_ability_with_cost(cost)),
                "{label}: nested Mill must disqualify"
            );
        }

        // Reach-guards: the same three shapes with PayLife in place of Mill.
        let composite_ok = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Tap,
                pay_life_one(),
            ],
        };
        let one_of_ok = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::OneOf {
                    costs: vec![
                        pay_life_one(),
                        AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                    ],
                },
            ],
        };
        let per_counter_ok = AbilityCost::PerCounter {
            counter: CounterType::Generic("charge".to_string()),
            target: TargetFilter::SelfRef,
            base: Box::new(pay_life_one()),
        };
        for (label, cost) in [
            ("Composite", composite_ok),
            ("OneOf nested in Composite", one_of_ok),
            ("PerCounter base", per_counter_ok),
        ] {
            assert!(
                is_mana_ability(&mana_ability_with_cost(cost)),
                "{label}: the non-library twin must stay a mana ability"
            );
        }
    }

    /// V2b — a cost on a **nested chain link** disqualifies. A root-only
    /// application of the cost criterion passes all three of these wrongly,
    /// because a `Mill` cost is not an `Effect` and no effect-shaped visitor can
    /// ever see it (CR 605.1a "its cost and effect" + CR 608.2c).
    #[test]
    fn mill_cost_on_a_chain_link_is_not_a_mana_ability() {
        let mill_link = || link(Effect::NoOp).cost(AbilityCost::Mill { count: 1 });
        let paid_link = || link(Effect::NoOp).cost(pay_life_one());

        let mut sub = colorless_tap_mana_ability();
        sub.sub_ability = Some(Box::new(mill_link()));
        assert!(!is_mana_ability(&sub), "sub_ability link cost");

        let mut els = colorless_tap_mana_ability();
        els.else_ability = Some(Box::new(mill_link()));
        assert!(!is_mana_ability(&els), "else_ability link cost");

        let mut modal = colorless_tap_mana_ability();
        modal.mode_abilities = vec![mill_link()];
        assert!(!is_mana_ability(&modal), "mode_abilities link cost");

        // Reach-guards: the same three links with a non-library cost. These
        // prove the walker reaches nested links at all, so the negatives above
        // are prunes of a real read rather than a miss.
        let mut sub_ok = colorless_tap_mana_ability();
        sub_ok.sub_ability = Some(Box::new(paid_link()));
        assert!(is_mana_ability(&sub_ok), "sub_ability link reached");

        let mut else_ok = colorless_tap_mana_ability();
        else_ok.else_ability = Some(Box::new(paid_link()));
        assert!(is_mana_ability(&else_ok), "else_ability link reached");

        let mut modal_ok = colorless_tap_mana_ability();
        modal_ok.mode_abilities = vec![paid_link()];
        assert!(is_mana_ability(&modal_ok), "mode_abilities link reached");
    }

    /// V2c — an `unless_pay` cost disqualifies. CR 118.12a routes the "unless
    /// [a player does something]" form into CR 118.12, which supplies "the
    /// action [do something] is a cost, **paid when the spell or ability
    /// resolves**" — so this arrives under CR 605.1a's *effect* limb via
    /// CR 608.2c, not under the CR 602.1a *activation cost* limb. Bare CR 118.12
    /// is the wrong citation for an "unless" form.
    #[test]
    fn unless_pay_mill_cost_is_not_a_mana_ability() {
        let mill_unless = crate::types::ability::UnlessPayModifier {
            cost: AbilityCost::Mill { count: 1 },
            payer: TargetFilter::Opponent,
        };
        let paid_unless = crate::types::ability::UnlessPayModifier {
            cost: pay_life_one(),
            payer: TargetFilter::Opponent,
        };

        assert!(
            !is_mana_ability(&colorless_tap_mana_ability().unless_pay(mill_unless.clone())),
            "CR 118.12a -> CR 118.12: an unless-pay Mill is a cost paid at resolution"
        );
        assert!(
            is_mana_ability(&colorless_tap_mana_ability().unless_pay(paid_unless.clone())),
            "reach-guard: the unless_pay leg is walked"
        );

        // Nested: an `unless_pay` on a chain link is reached too.
        assert!(!is_mana_ability(&mana_ability_with_sub(
            link(Effect::NoOp).unless_pay(mill_unless)
        )));
        assert!(is_mana_ability(&mana_ability_with_sub(
            link(Effect::NoOp).unless_pay(paid_unless)
        )));
    }

    /// V2e — the three **conditional** cost arms read their typed zone fields.
    ///
    /// This is the highest-consequence surface in the criterion, and it is the
    /// only one that fails DANGEROUS. Every other conditional fails safe (an
    /// ability wrongly keeps mana-ability status; zero cards affected today).
    /// These three fail by STRIPPING status: writing
    /// `AbilityCost::Exile { .. } => true` — dropping the zone read, a one-token
    /// slip — strips mana-ability status from 13 shipping cards: Elvish Spirit
    /// Guide, Simian Spirit Guide, Food Chain, Black Tulip, Cadaverous Bloom,
    /// Ether, Jack-o'-Lantern, Mirrored Lotus, Molt Tender, Rubble Rouser,
    /// Sunken Palace, Thornvault Forager, Titans' Nest.
    ///
    /// Both mutation directions are covered, and which assertion catches which
    /// is not symmetric:
    ///  - the **library** assertions fail under the `=> false` mutation;
    ///  - the **non-library** assertions fail under the `=> true` mutation.
    #[test]
    fn cost_axis_conditional_arms_read_their_typed_zone_fields() {
        // Library == disqualifying. Revert-failing for `=> false`.
        assert!(!is_mana_ability(&mana_ability_with_cost(exile_cost(Some(
            Zone::Library
        )))));
        assert!(!is_mana_ability(&mana_ability_with_cost(
            AbilityCost::ExileWithAggregate {
                filter: TargetFilter::SelfRef,
                function: crate::types::ability::AggregateFunction::Sum,
                property: crate::types::ability::ObjectProperty::ManaValue,
                comparator: Comparator::GE,
                value: 1,
                zone: Zone::Library,
            }
        )));
        assert!(!is_mana_ability(&mana_ability_with_cost(
            AbilityCost::ReturnToHand {
                count: 1,
                filter: None,
                from_zone: Some(Zone::Library),
            }
        )));

        // Non-library == still a mana ability. Revert-failing for `=> true`,
        // the strip-status direction, and therefore the PRIMARY guard for the
        // dangerous mutation — not optional decoration.
        //
        // `zone: None` is asserted EXPLICITLY and is the modal corpus value
        // (Black Tulip / Ether / Food Chain / Mirrored Lotus). It is `false`
        // because the classifier is static and cannot decide a missing zone on
        // EITHER payment path: `cost_payability::exile_cost_effective_zone` is
        // the authority for non-self costs only, and the `TargetFilter::SelfRef`
        // path short-circuits before it and resolves to the source's own current
        // zone (game state, which CR 605.2 forbids this classifier from
        // reading).
        for zone in [
            None,                    // black tulip / ether / food chain / mirrored lotus
            Some(Zone::Hand),        // elvish spirit guide / simian spirit guide
            Some(Zone::Graveyard),   // jack-o'-lantern / molt tender / titans' nest
            Some(Zone::Battlefield), // no shipping card, but the inferred default
        ] {
            assert!(
                is_mana_ability(&mana_ability_with_cost(exile_cost(zone))),
                "Exile {{ zone: {zone:?} }} must KEEP mana-ability status"
            );
        }
        assert!(is_mana_ability(&mana_ability_with_cost(
            AbilityCost::ExileWithAggregate {
                filter: TargetFilter::SelfRef,
                function: crate::types::ability::AggregateFunction::Sum,
                property: crate::types::ability::ObjectProperty::ManaValue,
                comparator: Comparator::GE,
                value: 1,
                zone: Zone::Graveyard,
            }
        )));
        // Grinning Ignus: `from_zone: None` means BATTLEFIELD.
        assert!(is_mana_ability(&mana_ability_with_cost(
            AbilityCost::ReturnToHand {
                count: 1,
                filter: None,
                from_zone: None,
            }
        )));
    }

    /// V3 — effect-side at the root `sub_ability` link. Chromatic Sphere:
    /// `{1}, {T}, Sacrifice this artifact: Add one mana of any color. Draw a
    /// card.`
    #[test]
    fn draw_in_sub_ability_is_not_a_mana_ability() {
        assert!(!is_mana_ability(&mana_ability_with_sub_effect(draw_one())));
        // Reach-guard: the identical fixture with Draw replaced by NoOp.
        assert!(is_mana_ability(&mana_ability_with_sub_effect(Effect::NoOp)));
    }

    /// V4 — effect-side at a **nested** `sub_ability` (depth >= 2). Deleting the
    /// recursive chain arm makes this pass wrongly.
    #[test]
    fn draw_at_nested_sub_ability_depth_is_not_a_mana_ability() {
        let mut inner = link(Effect::NoOp);
        inner.sub_ability = Some(Box::new(link(draw_one())));
        assert!(!is_mana_ability(&mana_ability_with_sub(inner)));

        let mut inner_ok = link(Effect::NoOp);
        inner_ok.sub_ability = Some(Box::new(link(Effect::NoOp)));
        assert!(is_mana_ability(&mana_ability_with_sub(inner_ok)));
    }

    /// V5 — effect-side at an `else_ability` link.
    #[test]
    fn draw_in_else_branch_is_not_a_mana_ability() {
        let mut def = colorless_tap_mana_ability();
        def.else_ability = Some(Box::new(link(draw_one())));
        assert!(!is_mana_ability(&def));

        let mut ok = colorless_tap_mana_ability();
        ok.else_ability = Some(Box::new(link(Effect::NoOp)));
        assert!(is_mana_ability(&ok));
    }

    /// V5b — effect-side in a `mode_abilities` entry.
    #[test]
    fn draw_in_a_mode_is_not_a_mana_ability() {
        let mut def = colorless_tap_mana_ability();
        def.mode_abilities = vec![link(draw_one())];
        assert!(!is_mana_ability(&def));

        let mut ok = colorless_tap_mana_ability();
        ok.mode_abilities = vec![link(Effect::NoOp)];
        assert!(is_mana_ability(&ok));
    }

    /// V6 — the criterion does NOT narrow ordinary mana abilities. This guards
    /// the over-narrowing direction across every shape the corpus actually
    /// carries, including a real `Exile`-cost card.
    #[test]
    fn library_criterion_does_not_narrow_ordinary_mana_abilities() {
        // Plain `{T}: Add {C}`.
        assert!(is_mana_ability(&colorless_tap_mana_ability()));
        // "Sacrifice a Goblin: Add {R}" — the existing builder.
        assert!(is_mana_ability(&skirk_prospector_mana_ability()));
        // Loot, the Pathfinder: `Exhaust — {G}, {T}: Add three mana of any one
        // color.` The only mana ability in the corpus carrying an `ability_tag`.
        let loot = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 3 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        })
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Tap,
            ],
        });
        assert!(is_mana_ability(&loot));

        // Elvish Spirit Guide: "Exile this creature from your hand: Add {G}."
        // NOTE the wording — "this **creature**", not "this card"; "Exile this
        // card from your hand" is SIMIAN Spirit Guide. Same AST shape either
        // way: `Exile { zone: Some(Hand), filter: Some(SelfRef) }`.
        let spirit_guide = |zone: Option<Zone>| {
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            })
            .cost(AbilityCost::Exile {
                count: 1,
                zone,
                filter: Some(TargetFilter::SelfRef),
            })
        };
        assert!(
            is_mana_ability(&spirit_guide(Some(Zone::Hand))),
            "Elvish Spirit Guide must remain a mana ability"
        );
        // Minimal one-field delta, so the pair isolates the zone read itself.
        assert!(
            !is_mana_ability(&spirit_guide(Some(Zone::Library))),
            "the same cost with zone=Library is disqualifying"
        );

        // Paired negative for each positive shape: add a Mill cost.
        for def in [
            colorless_tap_mana_ability(),
            skirk_prospector_mana_ability(),
            loot,
        ] {
            let base_cost = def.cost.clone().unwrap_or(AbilityCost::Tap);
            let milled = def.cost(AbilityCost::Composite {
                costs: vec![base_cost, AbilityCost::Mill { count: 1 }],
            });
            assert!(
                !is_mana_ability(&milled),
                "a Mill cost disqualifies every shape"
            );
        }
    }

    /// V7 — `Scry` does NOT disqualify, but `Surveil` does. The two keyword
    /// actions differ on exactly the axis under test, which is why they must
    /// never share an arm: CR 701.22a scry puts cards on the bottom or top of
    /// **your library** (every card starts and ends in the same library), while
    /// CR 701.25a surveil can put them **into your graveyard**.
    ///
    /// A real shipping card depends on this: The Secret Lair, `{T}, Say the
    /// secret word: Add one mana of any color. Scry 1. You gain 1 life.`
    #[test]
    fn scry_does_not_disqualify_but_surveil_does() {
        assert!(
            is_mana_ability(&mana_ability_with_sub_effect(scry_one())),
            "CR 701.22a: scry reorders WITHIN a library — The Secret Lair"
        );
        assert!(
            !is_mana_ability(&mana_ability_with_sub_effect(surveil_one())),
            "CR 701.25a: surveil can put cards into a graveyard"
        );
    }

    /// V8 — library-adjacent effects that move nothing to or from a library.
    #[test]
    fn library_reorder_reveal_and_other_decks_do_not_disqualify() {
        let benign = [
            // CR 701.24a: "randomize the cards WITHIN it".
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            // CR 701.20b: "Revealing a card doesn't cause it to leave the zone
            // it's in."
            Effect::RevealTop {
                player: TargetFilter::Controller,
                count: 1,
            },
            // CR 701.30a: the top card goes to the bottom or stays on top — of
            // its own library either way.
            Effect::Clash,
            // CR 901.4: plane and phenomenon cards remain in the COMMAND ZONE.
            Effect::ArrangePlanarDeckTop {
                count: QuantityExpr::Fixed { value: 2 },
                keep_on_top: QuantityExpr::Fixed { value: 1 },
            },
            // CR 701.51b + CR 717.2: the Attraction deck is in the command zone.
            Effect::OpenAttractions { count: 1 },
        ];
        for effect in benign {
            assert!(
                is_mana_ability(&mana_ability_with_sub_effect(effect.clone())),
                "{effect:?} moves no card to or from a library"
            );
        }
        // Paired negative in the same test: a Mill link in the same position.
        assert!(!is_mana_ability(&mana_ability_with_sub_effect(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            }
        )));
    }

    /// V9 — the registered-later boundary holds at a chain-link root. Barbed
    /// Sextant / Brass Infiniscope put their draw inside a delayed triggered
    /// ability (CR 603.7a), which goes on the stack later as its own object
    /// (CR 603.3), so it is not an instruction THIS ability follows (CR 608.2c).
    #[test]
    fn delayed_trigger_payload_is_not_this_abilitys_effect() {
        assert!(
            is_mana_ability(&mana_ability_with_sub_effect(delayed(draw_one()))),
            "CR 603.7a: a delayed trigger's payload is a separate ability"
        );
        // Reach-guard: Chromatic Sphere — the SAME Draw, not wrapped.
        assert!(
            !is_mana_ability(&mana_ability_with_sub_effect(draw_one())),
            "the unwrapped Draw in the same position DOES disqualify"
        );
    }

    /// V9b — the boundary holds at DEPTH >= 1. This is the central falsifier: a
    /// design that prunes only at chain-link roots and then delegates to an
    /// unscoped walker reaches the delayed trigger's payload through any inline
    /// branch carrier and wrongly disqualifies.
    #[test]
    fn boundary_holds_under_an_inline_choice_carrier() {
        let wrapped = delayed(draw_one());

        let carriers: Vec<(&str, Effect)> = vec![
            (
                "ChooseOneOf",
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![link(wrapped.clone())],
                },
            ),
            (
                "FlipCoin win branch",
                Effect::FlipCoin {
                    win_effect: Some(Box::new(link(wrapped.clone()))),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            ),
            (
                "RollDie result branch",
                Effect::RollDie {
                    count: QuantityExpr::Fixed { value: 1 },
                    sides: 20,
                    results: vec![crate::types::ability::DieResultBranch {
                        min: 1,
                        max: 20,
                        effect: Box::new(link(wrapped.clone())),
                    }],
                    modifier: None,
                },
            ),
            (
                "RevealFromHand on_decline",
                Effect::RevealFromHand {
                    filter: TargetFilter::Controller,
                    on_decline: Some(Box::new(link(wrapped.clone()))),
                },
            ),
        ];
        for (label, carrier) in &carriers {
            assert!(
                is_mana_ability(&mana_ability_with_sub_effect(carrier.clone())),
                "{label}: the boundary must hold one level down"
            );
        }

        // `AbilityCost::EffectCost` re-enters the effect walk from the cost
        // axis, so the scope must be threaded there too.
        assert!(is_mana_ability(&mana_ability_with_cost(
            AbilityCost::EffectCost {
                effect: Box::new(wrapped),
            }
        )));

        // Reach-guards: the same carriers with a BARE Draw, no wrapper. These
        // prove each carrier is descended at all, so the positives above are
        // boundary prunes rather than unreached subtrees.
        let bare_carriers: Vec<(&str, Effect)> = vec![
            (
                "ChooseOneOf",
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![link(draw_one())],
                },
            ),
            (
                "FlipCoin win branch",
                Effect::FlipCoin {
                    win_effect: Some(Box::new(link(draw_one()))),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            ),
            (
                "RollDie result branch",
                Effect::RollDie {
                    count: QuantityExpr::Fixed { value: 1 },
                    sides: 20,
                    results: vec![crate::types::ability::DieResultBranch {
                        min: 1,
                        max: 20,
                        effect: Box::new(link(draw_one())),
                    }],
                    modifier: None,
                },
            ),
            (
                "RevealFromHand on_decline",
                Effect::RevealFromHand {
                    filter: TargetFilter::Controller,
                    on_decline: Some(Box::new(link(draw_one()))),
                },
            ),
        ];
        for (label, carrier) in &bare_carriers {
            assert!(
                !is_mana_ability(&mana_ability_with_sub_effect(carrier.clone())),
                "{label}: reach-guard — the carrier IS descended"
            );
        }
        assert!(!is_mana_ability(&mana_ability_with_cost(
            AbilityCost::EffectCost {
                effect: Box::new(draw_one()),
            }
        )));
    }

    /// V9c — the boundary covers the replacement family, the emblem, and the
    /// token's granted abilities. Each REGISTERS something rather than moving a
    /// card during this resolution: CR 614.1 primary (a replacement applying to
    /// a later event or to another object is NOT a self-replacement effect under
    /// CR 614.15, so CR 605.1a's carve-out does not reach it), CR 114.1 for the
    /// emblem, CR 111.1 for the token, CR 611.2 for
    /// a granted continuous effect.
    #[test]
    fn replacement_emblem_and_token_payloads_are_not_this_abilitys_effect() {
        fn granting_static(effect: Effect) -> StaticDefinition {
            let mut def = StaticDefinition::new(StaticMode::Continuous);
            def.modifications = vec![ContinuousModification::GrantAbility {
                definition: Box::new(link(effect)),
            }];
            def
        }

        let wrapped: Vec<(&str, Effect)> = vec![
            (
                "CreateDrawReplacement",
                Effect::CreateDrawReplacement {
                    replacement_effect: Box::new(draw_one()),
                },
            ),
            (
                "CreateEmblem",
                Effect::CreateEmblem {
                    statics: vec![granting_static(draw_one())],
                    triggers: vec![],
                },
            ),
            (
                "GenericEffect granted ability",
                Effect::GenericEffect {
                    static_abilities: vec![granting_static(draw_one())],
                    duration: Some(Duration::UntilEndOfTurn),
                    target: None,
                    end_cost: None,
                },
            ),
            (
                "Token granted ability",
                Effect::Token {
                    name: "Test".to_string(),
                    power: crate::types::ability::PtValue::Fixed(1),
                    toughness: crate::types::ability::PtValue::Fixed(1),
                    types: vec!["Creature".to_string()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: vec![],
                    static_abilities: vec![granting_static(Effect::Mill {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        destination: Zone::Graveyard,
                    })],
                    enter_with_counters: vec![],
                },
            ),
        ];
        for (label, effect) in &wrapped {
            assert!(
                is_mana_ability(&mana_ability_with_sub_effect(effect.clone())),
                "{label}: the registered payload belongs to a later resolution \
                 or to another object"
            );
        }

        // Reach-guards: the unwrapped mover in the same chain position.
        assert!(!is_mana_ability(&mana_ability_with_sub_effect(draw_one())));
        assert!(!is_mana_ability(&mana_ability_with_sub_effect(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            }
        )));
    }

    /// V9d — inline carriers are STILL descended (guards over-pruning). These
    /// are branches of this resolution (CR 608.2c), not separate abilities.
    #[test]
    fn inline_carriers_are_still_descended() {
        let movers: Vec<(&str, Effect)> = vec![
            (
                "ChooseOneOf",
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![link(draw_one()), link(Effect::NoOp)],
                },
            ),
            (
                "FlipCoin lose branch",
                Effect::FlipCoin {
                    win_effect: None,
                    lose_effect: Some(Box::new(link(draw_one()))),
                    flipper: TargetFilter::Controller,
                },
            ),
            (
                "SeparateIntoPiles chosen pile",
                Effect::SeparateIntoPiles {
                    partition_subject: crate::types::ability::VoterScope::AllPlayers,
                    object_filter: TargetFilter::Controller,
                    chooser: PlayerScope::Controller,
                    chosen_pile_effect: Box::new(link(draw_one())),
                    pile_source: crate::types::ability::PileSource::Battlefield,
                    unchosen_pile_effect: None,
                },
            ),
            (
                "Vote outcome template",
                Effect::Vote {
                    choices: vec!["a".to_string(), "b".to_string()],
                    per_choice_effect: vec![
                        Box::new(link(draw_one())),
                        Box::new(link(Effect::NoOp)),
                    ],
                    starting_with: ControllerRef::You,
                    voter_scope: crate::types::ability::VoterScope::AllPlayers,
                    tally_mode: crate::types::ability::VoteTally::PerVote,
                    subject: crate::types::ability::VoteSubject::Named,
                    visibility: crate::types::ability::VoteVisibility::Open,
                },
            ),
        ];
        for (label, effect) in &movers {
            assert!(
                !is_mana_ability(&mana_ability_with_sub_effect(effect.clone())),
                "{label}: an inline branch is part of THIS resolution"
            );
        }

        // Reach-guards: the same carriers with NoOp in place of Draw.
        let benign: Vec<Effect> = vec![
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![link(Effect::NoOp), link(Effect::NoOp)],
            },
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: Some(Box::new(link(Effect::NoOp))),
                flipper: TargetFilter::Controller,
            },
            Effect::SeparateIntoPiles {
                partition_subject: crate::types::ability::VoterScope::AllPlayers,
                object_filter: TargetFilter::Controller,
                chooser: PlayerScope::Controller,
                chosen_pile_effect: Box::new(link(Effect::NoOp)),
                pile_source: crate::types::ability::PileSource::Battlefield,
                unchosen_pile_effect: None,
            },
            Effect::Vote {
                choices: vec!["a".to_string(), "b".to_string()],
                per_choice_effect: vec![Box::new(link(Effect::NoOp)), Box::new(link(Effect::NoOp))],
                starting_with: ControllerRef::You,
                voter_scope: crate::types::ability::VoterScope::AllPlayers,
                tally_mode: crate::types::ability::VoteTally::PerVote,
                subject: crate::types::ability::VoteSubject::Named,
                visibility: crate::types::ability::VoteVisibility::Open,
            },
        ];
        for effect in benign {
            assert!(is_mana_ability(&mana_ability_with_sub_effect(effect)));
        }
    }

    /// V10 — CR 603.12 reflexive links are excluded. Shaun & Rebecca, Agents:
    /// `{T}: Add {C}. When you do, mill two cards.` A reflexive triggered
    /// ability follows the rules for delayed triggered abilities (CR 603.7) and
    /// goes on the stack the next time a player would receive priority
    /// (CR 603.3) — the CR 603.12 exception is about WHEN the trigger condition
    /// is checked, not about when the ability resolves.
    #[test]
    fn reflexive_when_you_do_link_is_a_separate_ability() {
        let mill_two = || {
            link(Effect::Mill {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            })
        };

        let reflexive = mill_two().condition(AbilityCondition::WhenYouDo);
        assert!(
            is_mana_ability(&mana_ability_with_sub(reflexive)),
            "CR 603.12 -> CR 603.7 -> CR 603.3: a 'when you do' link is a \
             SEPARATE triggered ability"
        );

        // Reach-guard: the same chain with no condition at all.
        assert!(
            !is_mana_ability(&mana_ability_with_sub(mill_two())),
            "an unconditioned Mill link is part of this resolution"
        );

        // And the guard must key on `WhenYouDo` ALONE. "If you do, ..." is
        // CR 608.2c — one instruction conditional on another within the SAME
        // resolution — and must keep being descended. Widening the guard to the
        // engine's broader reflexive predicate (which unions the two because it
        // answers the skip-on-decline question) fails this assertion.
        let if_you_do = mill_two().condition(AbilityCondition::EffectOutcome {
            signal: crate::types::ability::EffectOutcomeSignal::OptionalEffectPerformed,
        });
        assert!(
            !is_mana_ability(&mana_ability_with_sub(if_you_do)),
            "an 'if you do' rider is CR 608.2c, not CR 603.12"
        );
    }

    /// V10b — the reflexive boundary holds on the COST axis too, because both
    /// walkers consult ONE authority (`scope_prunes_nested_ability`). Removing
    /// that call from the cost walker fails this row while leaving V10 green.
    #[test]
    fn reflexive_link_cost_is_also_excluded() {
        let reflexive_cost = link(Effect::NoOp)
            .cost(AbilityCost::Mill { count: 1 })
            .condition(AbilityCondition::WhenYouDo);
        assert!(
            is_mana_ability(&mana_ability_with_sub(reflexive_cost)),
            "the reflexive link's cost is the SEPARATE ability's cost"
        );

        // Reach-guard: the identical link without the condition (V2b's shape),
        // proving the cost walker reaches nested links at all — so the positive
        // above is a prune, not a miss.
        let plain_cost = link(Effect::NoOp).cost(AbilityCost::Mill { count: 1 });
        assert!(!is_mana_ability(&mana_ability_with_sub(plain_cost)));
    }

    /// V11 — `Effect::Mana`'s `grants` are deliberately NOT descended. Gilanra,
    /// Caller of Wirewood: `{T}: Add {G}. When you spend this mana to cast a
    /// spell with mana value 6 or greater, draw a card.` The rider is a
    /// `ManaSpellGrant::TriggerOnSpend` — CR 603.3, a separate triggered ability
    /// that fires when the mana is LATER spent, in a different resolution.
    ///
    /// `Effect::Mana` is the root of 100% of this classifier's inputs, so a
    /// "helpful" descent into `grants` here would misclassify Gilanra and
    /// Path of Ancestry. A future descent fails this test.
    #[test]
    fn mana_spend_grant_rider_is_a_separate_ability() {
        let gilanra = {
            let mut def = make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            });
            if let Effect::Mana { grants, .. } = &mut *def.effect {
                grants.push(crate::types::mana::ManaSpellGrant::TriggerOnSpend {
                    filter: TargetFilter::Any,
                    ability: Box::new(link(draw_one())),
                });
            } else {
                panic!("make_mana_ability must build an Effect::Mana");
            }
            def
        };
        assert!(
            is_mana_ability(&gilanra),
            "CR 603.3: a TriggerOnSpend rider is a separate triggered ability"
        );

        // Reach-guard: the SAME Draw moved from `grants` to a plain chain link.
        assert!(!is_mana_ability(&mana_ability_with_sub_effect(draw_one())));
    }

    /// V12 — the zone-conditional effect arms read their typed fields, and
    /// `Effect::Dig` is UNCONDITIONAL.
    ///
    /// `DigSource` is **not** a library-vs-not axis: under `PriorLook` the cards
    /// are still in `player.library` (the look-only pass takes an iterator slice
    /// and returns without removing them), so the library is the origin under
    /// BOTH variants. A `source ==` test here — or a test on
    /// `destination`/`rest_destination` — reproduces the same error on a
    /// different field.
    #[test]
    fn zone_conditional_arms_read_their_typed_fields() {
        fn dig(source: crate::types::ability::DigSource) -> Effect {
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                destination: None,
                keep_count: Some(1),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                rest_order: crate::types::ability::DigRestOrder::Preserve,
                reveal: false,
                enter_tapped: false,
                enters_attacking: false,
                source,
            }
        }
        fn change_zone(origin: Option<Zone>, destination: Zone, target: TargetFilter) -> Effect {
            Effect::ChangeZone {
                origin,
                destination,
                target,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            }
        }
        fn search(source_zones: Vec<Zone>) -> Effect {
            Effect::SearchLibrary {
                source_zones,
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
                split: None,
            }
        }
        fn counter(
            zone: Option<crate::types::ability::SpellStackToGraveyardReplacement>,
        ) -> Effect {
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: zone,
            }
        }
        fn pay_cost(cost: AbilityCost) -> Effect {
            Effect::PayCost {
                cost,
                scale: None,
                payer: TargetFilter::Controller,
            }
        }

        // Library-touching configurations disqualify.
        let disqualifying: Vec<(&str, Effect)> = vec![
            ("SearchLibrary[Library]", search(vec![Zone::Library])),
            (
                "ChangeZone destination=Library",
                change_zone(None, Zone::Library, TargetFilter::SelfRef),
            ),
            (
                "ChangeZone origin=Library",
                change_zone(Some(Zone::Library), Zone::Graveyard, TargetFilter::SelfRef),
            ),
            (
                "ChangeZone origin=None, zone in the filter",
                change_zone(
                    None,
                    Zone::Battlefield,
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature).properties(vec![
                        FilterProp::InZone {
                            zone: Zone::Library,
                        },
                    ])),
                ),
            ),
            (
                "Counter countered_spell_zone=Library",
                counter(Some(
                    crate::types::ability::SpellStackToGraveyardReplacement::Library {
                        position: crate::types::ability::LibraryPosition::Top,
                    },
                )),
            ),
            ("PayCost{Mill}", pay_cost(AbilityCost::Mill { count: 1 })),
            (
                "Dig{Library}",
                dig(crate::types::ability::DigSource::Library),
            ),
            (
                "Dig{PriorLook}",
                dig(crate::types::ability::DigSource::PriorLook),
            ),
        ];
        for (label, effect) in &disqualifying {
            assert!(
                !is_mana_ability(&mana_ability_with_sub_effect(effect.clone())),
                "{label} moves a card to or from a library"
            );
        }

        // Each CONDITIONAL arm with its non-library value — the reach-guards
        // that prove the arms are evaluated rather than hardcoded.
        let keeps: Vec<(&str, Effect)> = vec![
            ("SearchLibrary[Graveyard]", search(vec![Zone::Graveyard])),
            (
                "ChangeZone graveyard->battlefield",
                change_zone(Some(Zone::Graveyard), Zone::Battlefield, TargetFilter::Any),
            ),
            ("Counter{None}", counter(None)),
            ("PayCost{PayLife}", pay_cost(pay_life_one())),
            // For `Dig` the reach-guard is `Scry` — a genuine look-at-a-library
            // WITHOUT moving anything, which is the axis Dig actually differs
            // on. Round 2 used `Dig{PriorLook} => true` as this guard; that
            // pinned the wrong answer and is deliberately NOT reinstated.
            ("Scry", scry_one()),
        ];
        for (label, effect) in &keeps {
            assert!(
                is_mana_ability(&mana_ability_with_sub_effect(effect.clone())),
                "{label} must keep mana-ability status"
            );
        }
    }

    /// V12b — exactly ONE of `SpellStackToGraveyardReplacement`'s four carriers
    /// is read, and the asymmetry is the design.
    ///
    /// CR 605.1a scopes the criterion to "**its** cost and effect", so the
    /// question is not "does this field mention a library" but "whose resolution
    /// does the movement happen in".
    ///  - `Counter.countered_spell_zone` IS read. CR 608.2c cites Memory Lapse's
    ///    exact text ("Counter target spell. If that spell is countered this
    ///    way, put it on top of its owner's library instead of into its owner's
    ///    graveyard") as its OWN worked example of instructions this ability
    ///    follows; CR 701.6a puts the countered spell in the graveyard during
    ///    this resolution and the rider redirects that same event. Per CR 614.15
    ///    it is a SELF-replacement effect, which CR 605.1a's closing sentence
    ///    explicitly does NOT exclude.
    ///  - `FreeCastFromZones.graveyard_replacement` and
    ///    `CastingPermission::ExileWithAltCost.graveyard_replacement` are NOT
    ///    read. Each replaces the CAST SPELL'S OWN LATER RESOLUTION at its
    ///    CR 608.2n graveyard step ("as the final part of an instant or sorcery
    ///    spell's resolution"). That later resolution belongs to a different
    ///    object, so the rider is not this ability's own effect, so it is not a
    ///    self-replacement effect under CR 614.15, so CR 605.1a's closing
    ///    sentence says do not take it into account.
    ///
    /// Making the three arms symmetric fails this test, which is exactly its
    /// purpose. The configuration has ZERO cards today, so no census, coverage
    /// report, or card-level test can see it — this row is what makes the
    /// verdict durable against a later round re-deriving it.
    #[test]
    fn only_counter_reads_the_stack_to_graveyard_replacement() {
        use crate::types::ability::{
            CastingPermission, LibraryPosition, SpellStackToGraveyardReplacement,
        };

        let library_rider = || SpellStackToGraveyardReplacement::Library {
            position: LibraryPosition::Top,
        };
        let exile_with_alt_cost = |graveyard_replacement: Option<
            SpellStackToGraveyardReplacement,
        >| CastingPermission::ExileWithAltCost {
            cost: ManaCost::generic(0),
            cast_transformed: false,
            constraint: None,
            granted_to: None,
            resolution_cleanup: None,
            duration: None,
            graveyard_replacement,
            enters_with_counter: None,
            enters_with_modifications: vec![],
            mana_spend_permission: None,
        };
        let grant = |graveyard_replacement: Option<SpellStackToGraveyardReplacement>| {
            Effect::GrantCastingPermission {
                permission: exile_with_alt_cost(graveyard_replacement),
                target: TargetFilter::Any,
                grantee: crate::types::ability::PermissionGrantee::AbilityController,
            }
        };
        let free_cast =
            |zones: Vec<Zone>, graveyard_replacement: Option<SpellStackToGraveyardReplacement>| {
                Effect::FreeCastFromZones {
                    count: 1,
                    max_total_mv: None,
                    filter: TargetFilter::Any,
                    zones,
                    graveyard_replacement,
                }
            };

        // (1) `Counter`'s rider IS read: this ability's own resolution moves the
        // card from the stack to a library.
        assert!(!is_mana_ability(&mana_ability_with_sub_effect(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: Some(library_rider()),
            }
        )));
        // ... and its positive control: the same node with no rider.
        assert!(is_mana_ability(&mana_ability_with_sub_effect(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            }
        )));

        // (2) `FreeCastFromZones` reads `zones` ONLY.
        assert!(
            is_mana_ability(&mana_ability_with_sub_effect(free_cast(
                vec![Zone::Graveyard],
                Some(library_rider())
            ))),
            "graveyard_replacement is a rider on the CAST SPELL's later resolution"
        );
        // Positive control via the `zones` leg — proves the arm is reached and
        // genuinely discriminating rather than hardcoded `false`.
        assert!(
            !is_mana_ability(&mana_ability_with_sub_effect(free_cast(
                vec![Zone::Library],
                None
            ))),
            "the `zones` leg IS read"
        );

        // (3) `GrantCastingPermission` is not descended: the same answer with
        // and without the field, proving it is genuinely not consulted rather
        // than accidentally agreeing.
        assert!(is_mana_ability(&mana_ability_with_sub_effect(grant(Some(
            library_rider()
        )))));
        assert!(is_mana_ability(&mana_ability_with_sub_effect(grant(None))));

        // `GrantCastingPermission` is UNCONDITIONALLY false, so no input to it
        // can ever produce a `false` — both halves of the pair above assert
        // `true` and would also pass on a malformed fixture that never reached
        // the walked tree at all. This same-position control closes that hole:
        // a library mover at the identical depth MUST disqualify.
        assert!(
            !is_mana_ability(&mana_ability_with_sub_effect(
                Effect::PutAtLibraryPosition {
                    target: TargetFilter::SelfRef,
                    count: QuantityExpr::Fixed { value: 1 },
                    position: LibraryPosition::Top,
                }
            )),
            "positive control: the chain position the grant occupies IS walked"
        );
    }

    /// V13 — `is_renewable_mana_ability` is NOT narrowed by the library
    /// criterion. The divergence IS the assertion: a Millikin stops being a
    /// rules mana ability (CR 605.1a criterion 4) while remaining a manabase
    /// permanent, which is why the development predicate composes on
    /// `produces_mana_on_activation` and not on `is_mana_ability`.
    #[test]
    fn renewable_predicate_survives_the_library_criterion() {
        let millikin = mana_ability_with_cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Mill { count: 1 }],
        });
        assert!(
            !is_mana_ability(&millikin),
            "CR 605.1a criterion 4: Millikin's Mill cost disqualifies it"
        );
        assert!(
            is_renewable_mana_ability(&millikin),
            "but Millikin is still a standing manabase permanent — composing \
             the development predicate on the rules predicate would delete it \
             from manabase development and mulligan keep_tier"
        );
    }

    /// Row 4a — CR 701.21: one-shot self-sacrificing mana sources are NOT
    /// renewable. Gold is the constraint discriminator: its cost is a **bare**
    /// `Sacrifice` (not wrapped in a `Composite` like Treasure's), so a
    /// `Composite`-only implementation passes the Treasure assertion and fails
    /// this one. Driven through the production `predefined_token_abilities`
    /// materialization path, which is what actually lands on a live `GameObject`.
    #[test]
    fn treasure_and_gold_are_not_renewable_mana_abilities() {
        let treasure = crate::game::effects::token::predefined_token_abilities("Treasure");
        let gold = crate::game::effects::token::predefined_token_abilities("Gold");
        assert_eq!(treasure.len(), 1);
        assert_eq!(gold.len(), 1);

        // Positive control: both ARE mana abilities. This proves the exclusion
        // comes from the sacrifice clause and not from a failure to classify as
        // a mana ability at all.
        assert!(is_mana_ability(&treasure[0]));
        assert!(is_mana_ability(&gold[0]));

        assert!(
            !is_renewable_mana_ability(&treasure[0]),
            "Treasure ({{T}}, Sacrifice) is a one-shot source, not development"
        );
        assert!(
            !is_renewable_mana_ability(&gold[0]),
            "Gold's cost is a BARE Sacrifice — a Composite-only match misses it"
        );
    }

    /// Row 4b — CR 701.21a: sacrificing *another* permanent leaves the source on
    /// the battlefield, so a sac-outlet mana ability stays renewable. A
    /// filter-agnostic implementation (reusing `cost_includes_sacrifice` /
    /// `ManaSourcePenalty::Sacrifices`) fails this row.
    #[test]
    fn non_self_sacrifice_mana_outlet_stays_renewable() {
        assert!(
            is_renewable_mana_ability(&skirk_prospector_mana_ability()),
            "\"Sacrifice a Goblin: Add {{R}}\" keeps its source — still development"
        );

        // Paired negative: the identical shape with a SelfRef filter is excluded.
        let self_sac = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Red],
            contribution: ManaContribution::Base,
        })
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::SelfRef,
            1,
        )));
        assert!(!is_renewable_mana_ability(&self_sac));
    }

    /// A self-returning mana source (Grinning Ignus class) is a one-shot
    /// conversion from the standing manabase, whether its return cost is bare
    /// or composed with a tap cost.
    #[test]
    fn self_return_to_hand_mana_source_is_not_renewable() {
        let bare_return = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Red],
            contribution: ManaContribution::Base,
        })
        .cost(AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: None,
        });
        assert!(!is_renewable_mana_ability(&bare_return));

        let composite_return = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Red],
            contribution: ManaContribution::Base,
        })
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::ReturnToHand {
                    count: 1,
                    filter: Some(TargetFilter::SelfRef),
                    from_zone: None,
                },
            ],
        });

        assert!(!is_renewable_mana_ability(&composite_return));
    }

    /// Row 4c — Powerstone and Treasure are both artifact tokens differing only in
    /// cost shape (bare `Tap` vs `Composite{Tap, Sacrifice}`), so this proves the
    /// discriminator is the sacrifice clause rather than the token-ness.
    #[test]
    fn powerstone_is_a_renewable_mana_ability() {
        let powerstone = crate::game::effects::token::predefined_token_abilities("Powerstone");
        assert_eq!(powerstone.len(), 1);
        assert!(is_renewable_mana_ability(&powerstone[0]));
    }

    #[test]
    fn mana_api_type_detected_as_mana_ability() {
        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn is_mana_ability_serialized_only_when_true() {
        // The AbilityDefinition Serialize impl emits the derived `is_mana_ability`
        // UI key (skip_serializing_if = is_false), so the client routes mana-tap
        // affordances off this engine flag instead of introspecting the effect AST.
        let mana = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        let mana_json = serde_json::to_value(&mana).unwrap();
        assert_eq!(mana_json["is_mana_ability"], serde_json::json!(true));

        let non_mana = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        )
        .cost(AbilityCost::Tap);
        let non_mana_json = serde_json::to_value(&non_mana).unwrap();
        assert!(non_mana_json.get("is_mana_ability").is_none());
    }

    #[test]
    fn non_mana_api_type_not_detected() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        )
        .cost(AbilityCost::Tap);
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn targeted_mana_producing_ability_is_not_mana_ability() {
        // CR 605.1a: If a mana-producing ability has targets, it must use the stack.
        let mut def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        def.multi_target = Some(MultiTargetSpec::fixed(1, 1));
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn draw_ability_is_not_mana_ability() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn mana_with_delayed_trigger_sub_remains_mana_ability() {
        let mut head = make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 2 },
        });
        head.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(TriggerDefinition::new(TriggerMode::SpellCast)),
                    or_trigger: None,
                    lifetime: crate::types::ability::DelayedTriggerLifetime::ThisTurn,
                },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::CopySpell {
                        target: TargetFilter::TriggeringSource,
                        retarget: CopyRetargetPermission::KeepOriginalTargets,
                        copier: None,
                        additional_modifications: Vec::new(),
                        starting_loyalty_from_casualty_sacrifice: false,
                    },
                )),
                uses_tracked_set: false,
            },
        )));
        assert!(
            is_mana_ability(&head),
            "CR 605.1: chained delayed triggers do not disqualify activated mana abilities"
        );
    }

    #[test]
    fn resolve_mana_ability_sub_chain_registers_delayed_trigger() {
        use crate::types::ability::{
            CopyRetargetPermission, DelayedTriggerCondition, FilterProp, TriggerDefinition,
            TypedFilter,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Magus Lucea Kane".to_string(),
            Zone::Battlefield,
        );

        let copy_effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
        );
        let delayed = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new({
                        let mut trigger = TriggerDefinition::new(TriggerMode::SpellCast);
                        trigger.valid_card = Some(TargetFilter::Typed(
                            TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
                        ));
                        trigger.valid_target = Some(TargetFilter::Controller);
                        trigger
                    }),
                    or_trigger: None,
                    lifetime: crate::types::ability::DelayedTriggerLifetime::ThisTurn,
                },
                effect: Box::new(copy_effect),
                uses_tracked_set: false,
            },
        );
        let mut def = make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 2 },
        });
        def.sub_ability = Some(Box::new(delayed));

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.delayed_triggers.len(), 1);
        assert!(state.objects.get(&obj_id).unwrap().tapped);
    }

    #[test]
    fn mana_with_roll_die_sub_remains_mana_ability() {
        // CR 605.1: mana abilities remain mana abilities regardless of other
        // generated effects (Vexing Puzzlebox rolls a d20 inline).
        let mut def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
        )));
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn resolve_mana_ability_sub_chain_emits_die_rolled() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1011),
            PlayerId(0),
            "Vexing Puzzlebox".to_string(),
            Zone::Battlefield,
        );
        let mut def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
        )));
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, source, PlayerId(0), &def, &mut events, None).unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::DieRolled {
                    sides: 20,
                    result: Some(_),
                    ..
                }
            )),
            "RollDie sub_ability must resolve inline after mana production"
        );
    }

    #[test]
    fn mana_with_mana_sub_remains_mana_ability() {
        let mut def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Blue],
            contribution: ManaContribution::Base,
        });
        def.sub_ability = Some(Box::new(make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Red],
            contribution: ManaContribution::Base,
        })));
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn resolve_mana_ability_produces_mana_and_taps() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert!(state.objects.get(&obj_id).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn condition_instead_mana_ability_without_counter_produces_base_mana() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gemstone Caverns".to_string(),
            Zone::Battlefield,
        );
        let ability = gemstone_caverns_mana_ability();
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert_eq!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source).unwrap().tapped);
    }

    #[test]
    fn condition_instead_mana_ability_with_luck_counter_prompts_for_any_color() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gemstone Caverns".to_string(),
            Zone::Battlefield,
        );
        let ability = gemstone_caverns_mana_ability();
        let obj = state.objects.get_mut(&source).unwrap();
        obj.counters
            .insert(CounterType::Generic("luck".to_string()), 1);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            context,
        } = waiting
        else {
            panic!("expected ChooseManaColor, got {waiting:?}");
        };
        assert_eq!(player, PlayerId(0));
        assert_eq!(
            options,
            vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]
        );

        let pending = expect_mana_ability_context(context);
        handle_choose_mana_color(
            &mut state,
            &pending,
            &ManaChoicePrompt::SingleColor {
                options: options.clone(),
            },
            ManaChoice::SingleColor(ManaType::Blue),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            0
        );
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source).unwrap().tapped);
    }

    /// CR 305.2a + CR 608.2c + CR 605.1a + CR 605.3b: River of Tears, built end-
    /// to-end from its Oracle text via `parse_oracle_text`, swaps {U}→{B} at
    /// resolution exactly when the controller has played a land this turn. This
    /// exercises both branches of `apply_condition_instead_mana_swap` against the
    /// *parsed* AST (parser + runtime integration proof).
    #[test]
    fn river_of_tears_mana_swaps_blue_to_black_after_land_played() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "{T}: Add {U}. If you played a land this turn, add {B} instead.",
            "River of Tears",
            &[],
            &["Land".to_string()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1, "single mana ability");
        let ability = parsed.abilities[0].clone();

        let mut state = GameState::new_two_player(42);

        // No land played this turn (lands_played_this_turn == 0): base {U}.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "River of Tears".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability.clone());
        assert_eq!(state.players[0].lands_played_this_turn, 0);

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        assert_eq!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source).unwrap().tapped);

        // A land has now been played this turn: the {U}→{B} instead-swap fires.
        state.players[0].mana_pool.clear();
        state.players[0].lands_played_this_turn = 1;
        let source2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "River of Tears".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&source2).unwrap().abilities)
            .push(ability.clone());

        let mut events2 = Vec::new();
        activate_mana_ability(
            &mut state,
            source2,
            PlayerId(0),
            0,
            &ability,
            &mut events2,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source2).unwrap().tapped);
    }

    #[test]
    fn exhaust_mana_ability_only_once_is_enforced_and_emits_mana_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Loot, the Pathfinder".to_string(),
            Zone::Battlefield,
        );
        let mut def = AbilityDefinition::new(
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
        )
        .activation_restrictions(vec![ActivationRestriction::OnlyOnce]);
        def.ability_tag = Some(AbilityTag::Exhaust);
        Arc::make_mut(&mut state.objects.get_mut(&obj_id).unwrap().abilities).push(def.clone());

        let mut events = Vec::new();
        activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        let second = activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        );

        assert!(second.is_err());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Exhaust,
                player_id: PlayerId(0),
                source_id,
                is_mana_ability: true,
            } if *source_id == obj_id
        )));
    }

    #[test]
    fn exhaust_prompted_mana_ability_records_after_choice() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Exhaust Filter".to_string(),
            Zone::Battlefield,
        );
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Base,
                    color_options: vec![ManaColor::White, ManaColor::Blue],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .activation_restrictions(vec![ActivationRestriction::OnlyOnce]);
        def.ability_tag = Some(AbilityTag::Exhaust);
        Arc::make_mut(&mut state.objects.get_mut(&obj_id).unwrap().abilities).push(def.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::KeywordAbilityActivated { .. })));
        let WaitingFor::ChooseManaColor {
            choice, context, ..
        } = waiting
        else {
            panic!("expected ChooseManaColor");
        };
        let pending = expect_mana_ability_context(context);

        handle_choose_mana_color(
            &mut state,
            &pending,
            &choice,
            ManaChoice::SingleColor(ManaType::White),
            &mut events,
        )
        .unwrap();
        let second = activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        );

        assert!(second.is_err());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Exhaust,
                player_id: PlayerId(0),
                source_id,
                is_mana_ability: true,
            } if *source_id == obj_id
        )));
    }

    // CR 106.6: A mana ability that attaches a spend restriction (Flamebraider:
    // "Spend this mana only to cast Elemental spells or activate abilities of
    // Elemental sources") must thread that restriction onto every produced
    // `ManaUnit`. Previously `produce_mana_from_ability` destructured
    // `Effect::Mana { produced, .. }` and discarded `restrictions`, so the
    // mana landed in the pool unrestricted.
    #[test]
    fn resolve_mana_ability_attaches_spend_restrictions() {
        use crate::types::ability::ManaSpendRestriction;
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Flamebraider".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Fixed { value: 2 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                },
                restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                    spell_type: "Elemental".to_string(),
                    ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                }],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.total(), 2);
        // Every produced unit must carry the Elemental restriction.
        for unit in &pool.mana {
            assert_eq!(
                unit.restrictions,
                vec![
                    crate::types::mana::ManaRestriction::OnlyForTypeSpellsOrAbilities {
                        spell_type: "Elemental".to_string(),
                        ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                    }
                ],
                "Flamebraider mana must carry Elemental restriction"
            );
        }

        // Spending for a non-Elemental creature must fail.
        use crate::types::mana::{PaymentContext, SpellMeta};
        let goblin_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
        };
        let goblin_ctx = PaymentContext::Spell(&goblin_spell);
        let mut pool_clone = pool.clone();
        let first_color = pool_clone.mana[0].color;
        assert!(
            pool_clone.spend_for(first_color, &goblin_ctx).is_none(),
            "Flamebraider mana must not be spendable on non-Elemental spells"
        );

        // Spending for an Elemental creature succeeds.
        let elemental_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
        };
        let elemental_ctx = PaymentContext::Spell(&elemental_spell);
        assert!(
            pool_clone.spend_for(first_color, &elemental_ctx).is_some(),
            "Flamebraider mana must be spendable on Elemental spells"
        );

        // CR 106.6: The ability-activation half of the OR. A non-Elemental
        // source's activation context must reject Elemental-restricted mana;
        // an Elemental source's activation context must accept it.
        let non_elemental_types = vec!["Creature".to_string()];
        let non_elemental_subtypes = vec!["Goblin".to_string()];
        let non_elemental_activation = PaymentContext::Activation {
            source_types: &non_elemental_types,
            source_subtypes: &non_elemental_subtypes,
            ability_tag: None,
            mana_color_constraint: crate::types::mana::ActivationManaColorConstraint::Unrestricted,
        };
        let mut pool_clone2 = pool.clone();
        assert!(
            pool_clone2
                .spend_for(first_color, &non_elemental_activation)
                .is_none(),
            "Flamebraider mana must not pay non-Elemental source's ability cost"
        );

        let elemental_subtypes = vec!["Elemental".to_string()];
        let elemental_activation = PaymentContext::Activation {
            source_types: &non_elemental_types,
            source_subtypes: &elemental_subtypes,
            ability_tag: None,
            mana_color_constraint: crate::types::mana::ActivationManaColorConstraint::Unrestricted,
        };
        assert!(
            pool_clone2
                .spend_for(first_color, &elemental_activation)
                .is_some(),
            "Flamebraider mana must pay an Elemental source's ability cost"
        );
    }

    #[test]
    fn resolve_mana_ability_fails_if_already_tapped() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        let result = resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None);

        assert!(result.is_err());
    }

    #[test]
    fn resolve_mana_ability_colorless_produced() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    /// CR 614.1a positive case: the `Add {C}{C}{C} instead` sub-ability is a
    /// replacement effect (the word "instead" per CR 614.1a). When its `And`
    /// condition is satisfied (all three Urza lands controlled), the delta
    /// replaces the base `Add {C}` production and the pool ends with three
    /// colorless mana.
    #[test]
    fn resolve_mana_ability_conditional_urza_delta() {
        let mut state = GameState::new_two_player(42);
        let tower = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Urza's Tower".to_string(),
            Zone::Battlefield,
        );
        let mine = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Urza's Mine".to_string(),
            Zone::Battlefield,
        );
        let plant = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Urza's Power Plant".to_string(),
            Zone::Battlefield,
        );
        for (id, subtype) in [(tower, "Tower"), (mine, "Mine"), (plant, "Power-Plant")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Urza's".to_string());
            obj.card_types.subtypes.push(subtype.to_string());
        }

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .condition(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Mine".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Power-Plant".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                ],
            }),
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, tower, PlayerId(0), &ability, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            3
        );
    }

    /// CR 614.1a negative case: when the sub-ability's "instead" replacement
    /// (CR 614.1a) cannot fire because its condition is false (here, Urza's
    /// Power-Plant is missing), only the base `Add {C}` resolves and the
    /// `And { Mine, Power-Plant }` delta does not apply — the pool ends with
    /// one colorless, not three. Mirrors
    /// `resolve_mana_ability_conditional_urza_delta` but omits Power-Plant.
    #[test]
    fn resolve_mana_ability_urza_delta_skips_when_companion_land_missing() {
        let mut state = GameState::new_two_player(42);
        let tower = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Urza's Tower".to_string(),
            Zone::Battlefield,
        );
        let mine = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Urza's Mine".to_string(),
            Zone::Battlefield,
        );
        // Note: no Urza's Power Plant — the `And` condition cannot be
        // satisfied, so the sub-ability must not fire.
        for (id, subtype) in [(tower, "Tower"), (mine, "Mine")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Urza's".to_string());
            obj.card_types.subtypes.push(subtype.to_string());
        }

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .condition(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Mine".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Power-Plant".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                ],
            }),
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, tower, PlayerId(0), &ability, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1,
            "with Power-Plant absent the And condition is false and only the base \
             Add {{C}} fires; pool = {:?}",
            state.players[0].mana_pool.mana,
        );
    }

    #[test]
    fn resolve_mana_ability_fixed_multi_color_produces_each_unit() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hybrid Source".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::White, ManaColor::Blue],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn hand_self_exile_mana_ability_is_legal_and_exiles_source() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        let source = create_object(
            &mut state,
            CardId(157),
            player,
            "Elvish Spirit Guide".to_string(),
            Zone::Hand,
        );

        let mut ability = AbilityDefinition::new(
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
        )
        .cost(AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone: Some(Zone::Hand),
            count: 1,
        });
        ability.activation_zone = Some(Zone::Hand);
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability);

        let actions = crate::ai_support::legal_actions(&state);
        assert!(actions.iter().any(|action| matches!(
            action,
            crate::types::actions::GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == source
        )));

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .expect("hand-zone self-exile mana ability should activate");

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.objects[&source].zone, Zone::Exile);
        assert!(!state.players[0].hand.contains(&source));
    }

    #[test]
    fn resolve_composite_cost_taps_and_sacrifices() {
        // CR 111.10a + CR 605.3b: Treasure — Composite {Tap, Sacrifice} mana ability
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        // Mana was produced
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        // Object was sacrificed (moved out of battlefield)
        let obj = state.objects.get(&obj_id);
        assert!(
            obj.is_none() || obj.unwrap().zone != Zone::Battlefield,
            "Treasure should be sacrificed (removed from battlefield)"
        );
        // Events include both tap and sacrifice
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    /// CR 605.1a + CR 701.17a: Millikin — `{T}, Mill a card: Add {C}`. The mill
    /// is a non-mana cost component of a mana ability. Regression: before the
    /// `Mill` arm existed in the mana-ability cost payer, paying the cost errored
    /// (`Unsupported mana ability sub-cost: Mill`), so the readiness simulation
    /// in `can_activate_mana_ability_now` failed and the ability was never
    /// offered — the user could not tap Millikin for mana.
    ///
    /// **Premise note (CR 605.1a 2026 amendment):** the *ability-level* mill
    /// mechanics asserted below remain correct, but Millikin's ability is no
    /// longer reachable *as a mana ability* — its `Mill` cost moves a card from a
    /// library, so `is_mana_ability` now returns `false` for it. This test still
    /// passes because it drives the cost payer directly and never consults the
    /// classifier; see `mill_cost_is_not_a_mana_ability` for the classification.
    #[test]
    fn millikin_mills_a_card_and_adds_colorless() {
        let mut state = GameState::new_two_player(42);
        // Stock player 0's library so there is a card to mill.
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Island".to_string(),
            Zone::Library,
        );

        // Millikin is an artifact creature; mark it un-sick so the {T} gate passes.
        let id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Millikin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.summoning_sick = false;
        }

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Mill { count: 1 }],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());

        let library_before = state.players[0].library.len();

        // The user-facing regression: the ability must be offered as activatable.
        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), id, 0, &def),
            "Millikin's {{T}}, Mill a card: Add {{C}} ability must be activatable"
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        // {C} produced, source tapped, exactly one card milled to the graveyard.
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(state.objects.get(&id).unwrap().tapped);
        assert_eq!(state.players[0].library.len(), library_before - 1);
        assert_eq!(state.players[0].graveyard.len(), 1);
    }

    /// CR 118.3: Wall of Roots — `Put a -0/-1 counter on ~: Add {G}`. The cost is
    /// an `EffectCost` (put-counter-on-self), delegated to the single-authority
    /// cost payer. Regression: before the self-contained delegation arm, this
    /// errored and the ability was never offered.
    #[test]
    fn wall_of_roots_effect_cost_adds_green() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Wall of Roots".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
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
        )
        .cost(AbilityCost::EffectCost {
            effect: Box::new(Effect::PutCounter {
                counter_type: CounterType::PowerToughness {
                    power: 0,
                    toughness: -1,
                },
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }),
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        let counters = &state.objects.get(&id).unwrap().counters;
        assert_eq!(
            counters
                .get(&CounterType::PowerToughness {
                    power: 0,
                    toughness: -1
                })
                .copied(),
            Some(1),
            "the -0/-1 counter cost was paid onto Wall of Roots"
        );
    }

    /// CR 107.14: Aether Hub class — `{T}, Pay {E}: Add {C}`. The `PayEnergy`
    /// sub-cost delegates to the single-authority cost payer alongside the tap.
    #[test]
    fn energy_cost_mana_ability_spends_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 1;
        let id = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Aether Hub".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });
        // `can_activate_mana_ability_now` re-fetches the ability from the object,
        // so it must be attached at the activation index.
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());

        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), id, 0, &def),
            "energy-cost mana ability must be activatable with enough energy"
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert_eq!(state.players[0].energy, 0, "the {{E}} cost was spent");
    }

    /// CR 107.14: With insufficient energy the `PayEnergy` cost is unpayable
    /// (CR 118.3), so the ability is not offered.
    #[test]
    fn energy_cost_mana_ability_unavailable_without_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 0;
        let id = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Aether Hub".to_string(),
            Zone::Battlefield,
        );
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());
        assert!(
            !can_activate_mana_ability_now(&state, PlayerId(0), id, 0, &def),
            "energy-cost mana ability must NOT be activatable without energy"
        );
    }

    /// CR 107.6 + CR 302.6: Pili-Pala — `{Q}: Add {C}`. The untap symbol requires
    /// a currently-tapped source; once paid the source untaps. A summoning-sick
    /// creature can't activate it (CR 302.6 names {Q} alongside {T}).
    #[test]
    fn untap_cost_mana_ability_requires_tapped_source() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(33),
            PlayerId(0),
            "Pili-Pala".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.summoning_sick = false;
            obj.tapped = true;
        }

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Untap);
        // `can_activate_mana_ability_now` re-fetches the ability from the object.
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());

        // Tapped + un-sick: activatable.
        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            id,
            0,
            &def
        ));

        // Untapped: the {Q} cost can't be paid, so it's not offered.
        state.objects.get_mut(&id).unwrap().tapped = false;
        assert!(!can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            id,
            0,
            &def
        ));

        // Summoning sick (while tapped): CR 302.6 blocks {Q}.
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.tapped = true;
            obj.summoning_sick = true;
        }
        assert!(!can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            id,
            0,
            &def
        ));

        // Pay it: untaps and produces {C}.
        state.objects.get_mut(&id).unwrap().summoning_sick = false;
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(!state.objects.get(&id).unwrap().tapped, "{{Q}} untapped it");
    }

    /// CR 118.3 + CR 602.2b: Grinning Ignus — `Return ~ to its owner's hand:
    /// Add {C}`. The self-`ReturnToHand` cost delegates to the single-authority
    /// cost payer; the source ends up in hand.
    #[test]
    fn self_return_to_hand_cost_mana_ability() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(34),
            PlayerId(0),
            "Grinning Ignus".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: None,
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert_eq!(
            state.objects.get(&id).unwrap().zone,
            Zone::Hand,
            "source returned to hand as the cost"
        );
    }

    /// CR 701.43: Oasis Ritualist class — `{T}, Exert ~: Add {C}`. The `Exert`
    /// sub-cost delegates to the single-authority cost payer alongside the tap.
    #[test]
    fn exert_cost_mana_ability_taps_and_produces() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(35),
            PlayerId(0),
            "Oasis Ritualist".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().summoning_sick = false;

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Exert],
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(state.objects.get(&id).unwrap().tapped);
    }

    /// CR 605.1a: A loyalty ability that adds mana (Chandra `[+1]: Add {R}{R}`)
    /// is NOT a mana ability — it uses the stack and obeys loyalty timing. The
    /// classifier must exclude it; an otherwise-identical `{T}` cost stays a mana
    /// ability.
    #[test]
    fn loyalty_ability_is_not_a_mana_ability() {
        let mana_effect = || Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Red, ManaColor::Red],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };

        let loyalty_ability = AbilityDefinition::new(AbilityKind::Activated, mana_effect())
            .cost(AbilityCost::Loyalty { amount: 1 });
        assert!(
            !is_mana_ability(&loyalty_ability),
            "a [+1]: Add {{R}}{{R}} loyalty ability is not a mana ability (CR 605.1a)"
        );

        let tap_ability =
            AbilityDefinition::new(AbilityKind::Activated, mana_effect()).cost(AbilityCost::Tap);
        assert!(
            is_mana_ability(&tap_ability),
            "the same effect with a {{T}} cost remains a mana ability"
        );
    }

    /// Build a Treasure-style token — `{T}, Sacrifice this: Add one mana of any
    /// color` over `colors` — attached as ability index 0. The
    /// `Composite { Tap, Sacrifice SelfRef }` cost is choice-free, so two
    /// definition-identical copies are batchable twins (CR 605.3a).
    fn make_any_color_treasure(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        colors: Vec<ManaColor>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: colors,
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
        id
    }

    /// Build a creature with a pure `{T}: Add one mana of any color` ability at
    /// index 0. Unlike `make_any_color_treasure`, the Creature core type makes it
    /// subject to the CR 302.6 summoning-sickness gate, so `summoning_sick`
    /// controls whether the `{T}` mana ability is currently ready.
    fn make_tap_any_color_creature(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        summoning_sick: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            "Mana Dork".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.summoning_sick = summoning_sick;
        }
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: ManaColor::ALL.to_vec(),
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
        id
    }

    #[test]
    fn token_treasure_choose_color_uses_activation_snapshot_after_self_sacrifice() {
        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9000, PlayerId(0), ManaColor::ALL.to_vec());
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.is_token = true;
            let ability = Arc::make_mut(&mut obj.abilities).get_mut(0).unwrap();
            let Effect::Mana {
                produced: ManaProduction::AnyOneColor { count, .. },
                ..
            } = ability.effect.as_mut()
            else {
                panic!("test treasure must have AnyOneColor mana production");
            };
            *count = QuantityExpr::Fixed { value: 2 };
        }

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
            },
        )
        .expect("token Treasure should activate into a color prompt");
        assert!(
            matches!(result.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "Goldspan-style Treasure should wait for a color choice"
        );
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);
        assert!(
            !state.objects.contains_key(&treasure),
            "a sacrificed token Treasure has ceased to exist before the color choice"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
        )
        .expect("color choice must resolve from the activation-time ability snapshot");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            2,
            "Goldspan-style Treasure adds two mana of the chosen color"
        );
    }

    /// CR 605.3a: One color choice with `count = N` activates the tapped source
    /// plus `N - 1` identical, choice-free twins — `N` mana of the chosen color,
    /// `N` sources sacrificed, and a per-source tap each twin (the events a
    /// sacrifice observer such as Mayhem Devil/Korvold sees).
    #[test]
    fn batch_activation_taps_multiple_identical_treasures() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9001, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9002, PlayerId(0), ManaColor::ALL.to_vec());
        let c = make_any_color_treasure(&mut state, 9003, PlayerId(0), ManaColor::ALL.to_vec());

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("Treasure should activate into a color prompt");

        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &result.waiting_for
        else {
            panic!("expected ChooseManaColor, got {:?}", result.waiting_for);
        };
        assert_eq!(
            pending.batch_siblings,
            vec![b, c],
            "the other two Treasures are batchable twins"
        );

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 3,
            },
        )
        .expect("bulk color choice should resolve");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            3,
            "three Treasures each produced one red"
        );
        let on_battlefield = [a, b, c]
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.zone == Zone::Battlefield)
            })
            .count();
        assert_eq!(on_battlefield, 0, "all three Treasures were sacrificed");
        // CR 106.12 + CR 605.3a: each twin taps independently during the choice
        // step (the first source was tapped earlier, before the prompt).
        let twin_taps = result
            .events
            .iter()
            .filter(|e| matches!(e, GameEvent::PermanentTapped { .. }))
            .count();
        assert_eq!(twin_taps, 2, "two twins tapped during the bulk activation");
    }

    /// CR 605.3a: Activating one of `N` identical batchable Treasures must compute
    /// the sibling set in linear time. Pre-fix, `batch_eligible_siblings` filtered
    /// each candidate through `activatable_mana_options`, which cloned + simulated +
    /// recursed `can_activate_mana_ability_now`, giving O(N!) readiness calls. The
    /// single-authority non-simulating predicate caps the count at O(N).
    #[test]
    fn bulk_treasure_activation_is_linear_not_factorial() {
        const N: usize = 6;
        let mut state = GameState::new_two_player(42);
        let ids: Vec<ObjectId> = (0..N)
            .map(|i| {
                make_any_color_treasure(
                    &mut state,
                    9200 + i as u64,
                    PlayerId(0),
                    ManaColor::ALL.to_vec(),
                )
            })
            .collect();

        MANA_READINESS_CALLS.store(0, Ordering::Relaxed);
        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: ids[0],
                ability_index: 0,
            },
        )
        .expect("Treasure should activate into a color prompt");

        // O(N!) pre-fix blows far past this; O(N) post-fix stays well under it.
        // The bound carries slack for parallel-schedule noise: MANA_READINESS_CALLS
        // is a bare process-global, so any CONCURRENTLY-running test that exercises
        // mana readiness increments it between this test's store(0) and load. The
        // detector's discrimination survives the slack — the O(N!) regression this
        // guards against produces >= 6! = 720 calls at N = 6, 15x over this bound,
        // while observed schedule pollution is single-digit.
        assert!(
            MANA_READINESS_CALLS.load(Ordering::Relaxed) <= 8 * N,
            "readiness calls must be linear in N (got {}, bound {})",
            MANA_READINESS_CALLS.load(Ordering::Relaxed),
            8 * N
        );

        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &result.waiting_for
        else {
            panic!("expected ChooseManaColor, got {:?}", result.waiting_for);
        };
        assert_eq!(
            pending.batch_siblings,
            ids[1..].to_vec(),
            "the other five Treasures are batchable twins"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: N as u32,
            },
        )
        .expect("bulk color choice should resolve");
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            N,
            "all six Treasures each produced one red"
        );
    }

    /// CR 106.12: A tapped `{T}: Add` source can't pay its tap cost, so the cheap
    /// gate (`mana_ability_ready_without_simulation`) rejects it BEFORE the skip
    /// shortcut and before any legality clone — A(a).
    #[test]
    fn cheap_gate_rejects_tapped_tap_mana_source_without_clone() {
        let mut state = GameState::new_two_player(42);
        let dork = make_tap_any_color_creature(&mut state, 9300, PlayerId(0), false);
        state.objects.get_mut(&dork).unwrap().tapped = true;
        let def = state.objects.get(&dork).unwrap().abilities[0].clone();

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), dork, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            !activatable,
            "a tapped {{T}} mana source can't pay its tap cost (CR 106.12)"
        );
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "cheap gate rejects before any legality clone"
        );
    }

    /// CR 604.1: Make the O(1) `StaticModePresence` index precise, then zero the
    /// perf counters. **Every self-sacrifice cheap-gate assertion below must go
    /// through this.** A fresh `GameState` seeds
    /// `StaticModePresence::all_present()`, so `legality_simulation_is_redundant`'s
    /// `CantPayCost` presence guard declines the fast path until the layers
    /// pipeline has flushed — a test that skips the flush measures an inert fast
    /// path and its `== 0` clone assertion fails. Production always flushes first
    /// (`public_state::finalize_rules_state` -> `finalize_display_state`), so this
    /// mirrors production rather than papering over it.
    fn flush_and_reset(state: &mut GameState) {
        crate::game::layers::flush_layers(state);
        crate::game::perf_counters::reset();
    }

    /// CR 701.21a: the bare self-sacrifice cost component — "Sacrifice this".
    fn self_sacrifice_cost() -> AbilityCost {
        AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1))
    }

    /// CR 111.10a: Treasure's `{T}, Sacrifice this token` cost tree.
    fn tap_and_self_sacrifice_cost() -> AbilityCost {
        AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, self_sacrifice_cost()],
        }
    }

    fn any_one_color(count: QuantityExpr) -> ManaProduction {
        ManaProduction::AnyOneColor {
            count,
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        }
    }

    /// Attach one activated mana ability (`cost` -> `produced`) to a fresh
    /// battlefield object at ability index 0. Single builder for the
    /// self-sacrifice cheap-gate fixtures, so each test below states only the
    /// axis it actually varies.
    fn spawn_mana_source(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        name: &str,
        cost: AbilityCost,
        produced: ManaProduction,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(cost);
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
        id
    }

    /// CR 118.3: install a global "players can't sacrifice a creature to pay a
    /// cost" static (Yasharn class) on its own battlefield permanent. Fixture
    /// shape mirrors `sacrifice_mana_cost_rejects_prohibited_selected_permanent`.
    fn install_cant_sacrifice_creature_static(state: &mut GameState, card: u64, player: PlayerId) {
        let lock = create_object(
            state,
            CardId(card),
            player,
            "Cost Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantPayCost {
                who: ProhibitionScope::AllPlayers,
                cost: CostPaymentProhibition::Sacrifice {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            }));
    }

    /// CR 111.10a + CR 701.21a: A Treasure's `Composite{{Tap, Sacrifice this}}`
    /// mana cost IS conclusively decided without simulating. The sacrifice target
    /// is the ability's own source, so — behind the two state-aware guards in
    /// `legality_simulation_is_redundant` — it is exactly as deterministic as the
    /// bare `{T}` cost the cheap gate already skips.
    ///
    /// A(b), **rewritten**: this test previously pinned the pre-fix `clone >= 1`
    /// behavior, which is the behavior being changed.
    ///
    /// REVERT-PROBE: drop the `has_unambiguous_self_sacrifice_component` disjunct
    /// from `legality_simulation_is_redundant` and `state_clone_for_legality`
    /// returns to 1 while `activatable` stays true.
    #[test]
    fn composite_tap_self_sacrifice_skips_legality_clone() {
        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9301, PlayerId(0), ManaColor::ALL.to_vec());
        let def = state.objects.get(&treasure).unwrap().abilities[0].clone();

        flush_and_reset(&mut state);
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), treasure, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "an untapped Treasure with a legal self-sacrifice is activatable \
             (positive reach-guard: a 0-clone count is meaningless if the source \
             was rejected upstream by the readiness gate)"
        );
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "a whole-tree choice-free self-sacrifice cost is conclusively payable \
             (CR 111.10a + CR 701.21a) — no legality clone"
        );
    }

    /// CR 111.10c: Gold's tapless `Sacrifice this token: Add one mana of any
    /// color` also skips the clone. The design deliberately composes
    /// `has_unambiguous_self_sacrifice_component` (which requires a self-sacrifice
    /// component to be present) rather than the `{T}`/`{Q}` anchor, so the tapless
    /// half of the class is in — a *tapped* Gold token genuinely can still be
    /// sacrificed for mana.
    ///
    /// REVERT-PROBE: re-anchor the fast path on `has_tap_component` and this goes
    /// red while `composite_tap_self_sacrifice_skips_legality_clone` stays green.
    #[test]
    fn tapless_self_sacrifice_skips_legality_clone() {
        let mut state = GameState::new_two_player(42);
        let gold = spawn_mana_source(
            &mut state,
            9310,
            PlayerId(0),
            "Gold",
            self_sacrifice_cost(),
            any_one_color(QuantityExpr::Fixed { value: 1 }),
        );
        // CR 106.12: a tapped source with no {T} component is still payable —
        // the readiness gate's tapped check is gated on `has_tap_component`.
        state.objects.get_mut(&gold).unwrap().tapped = true;
        let def = state.objects.get(&gold).unwrap().abilities[0].clone();

        flush_and_reset(&mut state);
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), gold, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "a tapped Gold token can still pay its tapless self-sacrifice cost \
             (CR 111.10c) — positive reach-guard for the clone count below"
        );
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "a tapless self-sacrifice cost is conclusively payable — no legality clone"
        );
    }

    /// **The direct discharge of "the fast path must not change the ANSWER."**
    ///
    /// For every shape the fast path now skips, the skipped simulation is run
    /// explicitly and asserted to return the same `true` — so the design rests on
    /// a measurement rather than on the assumption that the simulation would have
    /// agreed. This is an equivalence test: it passes before and after the change
    /// by construction, and it is the anti-vacuity backstop for U1/U2.
    ///
    /// Shapes (d) and (e) are the **Lotus Blossom class** (`lotus blossom`,
    /// `glittering stockpile`, `shrine of boundless growth`): the produced amount
    /// is `CountersOn { scope: Source }`, so the production tail reads the source
    /// **after** `sacrifice_permanent` has already moved it to the graveyard —
    /// last known information per CR 608.2h. (e) is the boundary where that read
    /// yields zero. Either way the tail is infallible, so the legality answer
    /// stays `true`; only the *amount* of mana can differ.
    #[test]
    fn self_sacrifice_fast_path_answer_matches_simulation() {
        let assert_agrees =
            |state: &GameState, id: ObjectId, def: &AbilityDefinition, label: &str| {
                assert!(
                    can_activate_mana_ability_by_simulation(state, PlayerId(0), id, 0, def),
                    "{label}: the simulation the fast path skips must itself answer true"
                );
                assert!(
                    can_activate_mana_ability_now(state, PlayerId(0), id, 0, def),
                    "{label}: the fast path must report the simulation's answer"
                );
            };

        // (a) Treasure — `{T}, Sacrifice this` -> one mana of any color.
        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9320, PlayerId(0), ManaColor::ALL.to_vec());
        let def = state.objects.get(&treasure).unwrap().abilities[0].clone();
        crate::game::layers::flush_layers(&mut state);
        assert_agrees(&state, treasure, &def, "Treasure {T} + self-sacrifice");

        // (b) Gold — tapless `Sacrifice this`.
        let mut state = GameState::new_two_player(42);
        let gold = spawn_mana_source(
            &mut state,
            9321,
            PlayerId(0),
            "Gold",
            self_sacrifice_cost(),
            any_one_color(QuantityExpr::Fixed { value: 1 }),
        );
        let def = state.objects.get(&gold).unwrap().abilities[0].clone();
        crate::game::layers::flush_layers(&mut state);
        assert_agrees(&state, gold, &def, "Gold tapless self-sacrifice");

        // (c) Colorless production — no color prompt, so the simulation runs the
        // whole post-sacrifice production tail instead of parking on a choice.
        let mut state = GameState::new_two_player(42);
        let scion = spawn_mana_source(
            &mut state,
            9322,
            PlayerId(0),
            "Eldrazi Scion",
            self_sacrifice_cost(),
            ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
        );
        let def = state.objects.get(&scion).unwrap().abilities[0].clone();
        crate::game::layers::flush_layers(&mut state);
        assert_agrees(&state, scion, &def, "colorless self-sacrifice production");

        // (d) CR 608.2h — source-referential produced amount, counters PRESENT.
        for (counters, label) in [
            (3u32, "Lotus Blossom class, 3 petal counters"),
            (0u32, "Lotus Blossom class, ZERO petal counters"),
        ] {
            let mut state = GameState::new_two_player(42);
            let blossom = spawn_mana_source(
                &mut state,
                9323,
                PlayerId(0),
                "Lotus Blossom",
                tap_and_self_sacrifice_cost(),
                any_one_color(QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(CounterType::Generic("petal".to_string())),
                    },
                }),
            );
            if counters > 0 {
                state
                    .objects
                    .get_mut(&blossom)
                    .unwrap()
                    .counters
                    .insert(CounterType::Generic("petal".to_string()), counters);
            }
            let def = state.objects.get(&blossom).unwrap().abilities[0].clone();
            crate::game::layers::flush_layers(&mut state);
            assert_agrees(&state, blossom, &def, label);
        }
    }

    /// CR 701.21a: a **non-self** `Sacrifice` target stays OUT of the fast path —
    /// a legal victim may not exist, so its simulation is load-bearing. Phyrexian
    /// Altar shape, with a legal victim on the board so the readiness gate passes
    /// and the decision seam is genuinely reached.
    ///
    /// The `activatable == true` assertion is the paired positive control: it
    /// proves readiness passed, so `>= 1` measures the fast path declining rather
    /// than an upstream rejection.
    #[test]
    fn non_self_sacrifice_mana_cost_still_simulates() {
        let mut state = GameState::new_two_player(42);
        let altar = spawn_mana_source(
            &mut state,
            9330,
            PlayerId(0),
            "Phyrexian Altar",
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(
                        TargetFilter::Typed(TypedFilter::creature()),
                        1,
                    )),
                ],
            },
            any_one_color(QuantityExpr::Fixed { value: 1 }),
        );
        let victim = create_object(
            &mut state,
            CardId(9331),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let def = state.objects.get(&altar).unwrap().abilities[0].clone();

        flush_and_reset(&mut state);
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), altar, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "a legal victim is on the board, so readiness passes and the decision \
             seam is really reached (positive control for the clone count)"
        );
        assert!(
            snap.state_clone_for_legality >= 1,
            "a non-self Sacrifice target must still simulate — the victim's \
             existence is not settled by the cost's AST shape"
        );
    }

    /// **Guard 1 — CR 601.2g.** A permanent already committed to a pending
    /// spell's additional sacrifice cost is reserved: paying this ability's cost
    /// would error at `continue_mana_ability_cost_payment_in_node`, so the fast
    /// path must decline and let the simulation report `false`.
    ///
    /// Multi-authority fixture: two definition-identical Treasures on one board,
    /// exactly one reserved. A hoisted or board-global guard fails this test,
    /// because the guard is keyed on `source_id`.
    ///
    /// **Non-vacuity:** the unreserved sibling reporting `true` is what proves the
    /// reservation was really installed — `PendingCast::new` seeds
    /// `deferred_sacrificed_permanents` **empty**, so a fixture that only installs
    /// a `PendingCast` reserves nothing and both sources would report `true`.
    ///
    /// REVERT-PROBE: delete the `cost_sacrifices_reserved_source` term and the
    /// reserved source flips to `true` with 0 clones.
    #[test]
    fn self_sacrifice_reserved_for_pending_cast_still_simulates() {
        use crate::types::game_state::{DeferredSacrificeSelection, PendingCast};

        let mut state = GameState::new_two_player(42);
        let reserved =
            make_any_color_treasure(&mut state, 9340, PlayerId(0), ManaColor::ALL.to_vec());
        let sibling =
            make_any_color_treasure(&mut state, 9341, PlayerId(0), ManaColor::ALL.to_vec());
        let spell = create_object(
            &mut state,
            CardId(9342),
            PlayerId(0),
            "Some Spell".to_string(),
            Zone::Stack,
        );
        let mut pending = PendingCast::new(
            spell,
            CardId(9342),
            ResolvedAbility::new(
                Effect::unimplemented("Some Spell", "test fixture"),
                Vec::new(),
                spell,
                PlayerId(0),
            ),
            ManaCost::generic(1),
        );
        // CR 601.2g: `deferred_spell_sacrifice_reserved` matches on `object_id`
        // alone, and `PendingCast::new` seeds this vector empty — the reservation
        // must be pushed explicitly or the test passes vacuously.
        pending
            .deferred_sacrificed_permanents
            .push(DeferredSacrificeSelection {
                object_id: reserved,
                filter: TargetFilter::Typed(TypedFilter::permanent()),
            });
        state.pending_cast = Some(Box::new(pending));

        let def = state.objects.get(&reserved).unwrap().abilities[0].clone();

        flush_and_reset(&mut state);
        let reserved_activatable =
            can_activate_mana_ability_now(&state, PlayerId(0), reserved, 0, &def);
        let reserved_snap = crate::game::perf_counters::snapshot();

        crate::game::perf_counters::reset();
        let sibling_activatable =
            can_activate_mana_ability_now(&state, PlayerId(0), sibling, 0, &def);
        let sibling_snap = crate::game::perf_counters::snapshot();

        assert!(
            !reserved_activatable,
            "a Treasure reserved for a pending spell's additional sacrifice cost \
             can't also pay this mana ability's cost (CR 601.2g)"
        );
        assert!(
            reserved_snap.state_clone_for_legality >= 1,
            "the reserved source declines the fast path and simulates"
        );
        assert!(
            sibling_activatable,
            "the definition-identical UNRESERVED sibling on the same board is \
             still activatable — this is the non-vacuity guard proving the \
             reservation was really installed"
        );
        assert_eq!(
            sibling_snap.state_clone_for_legality, 0,
            "the guard is per-source_id, not board-global: the sibling still \
             takes the fast path"
        );
    }

    /// **Guard 2 — CR 118.3 + CR 601.2h, non-vacuously.** The readiness gate
    /// evaluates `player_cant_sacrifice_as_cost` on the PRE-payment state, but the
    /// payment re-evaluates it after this cost tree's `{T}` component has already
    /// tapped the source, and a prohibition's object filter can read that tapped
    /// bit (`FilterProp::Tapped`). So whenever any `CantPayCost` static is
    /// functioning, the fast path declines and simulates.
    ///
    /// The static here filters **creatures**, which does not match the artifact
    /// Treasure, so readiness still passes and the decision seam is genuinely
    /// reached — the guard is measured, not inferred from an upstream rejection.
    ///
    /// REVERT-PROBE: delete the `static_kind_present` term and the main arm's
    /// clone count drops to 0.
    #[test]
    fn cant_pay_cost_static_presence_declines_self_sacrifice_fast_path() {
        // Main arm: the static IS present.
        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9350, PlayerId(0), ManaColor::ALL.to_vec());
        install_cant_sacrifice_creature_static(&mut state, 9351, PlayerId(0));
        let def = state.objects.get(&treasure).unwrap().abilities[0].clone();

        flush_and_reset(&mut state);
        // Non-vacuity FIRST: `static_kind_present` is an O(1) absence
        // short-circuit, so an unflushed or mis-built board would let the
        // assertions below pass for free.
        assert!(
            static_kind_present(&state, StaticModeKind::CantPayCost),
            "the CantPayCost static must be functioning, or Guard 2 is untested"
        );
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), treasure, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "the prohibition filters creatures, so an artifact Treasure is still \
             activatable — readiness passed and the decision seam was reached"
        );
        assert!(
            snap.state_clone_for_legality >= 1,
            "Guard 2 declines the fast path while any CantPayCost static is present"
        );

        // Paired positive control: the identical board WITHOUT the static.
        let mut control = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut control, 9350, PlayerId(0), ManaColor::ALL.to_vec());
        let def = control.objects.get(&treasure).unwrap().abilities[0].clone();

        flush_and_reset(&mut control);
        assert!(
            !static_kind_present(&control, StaticModeKind::CantPayCost),
            "control board carries no CantPayCost static"
        );
        let activatable = can_activate_mana_ability_now(&control, PlayerId(0), treasure, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(activatable, "control Treasure is activatable");
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "without the static the same board takes the fast path — this is what \
             makes the `>= 1` above a measurement rather than a broken board"
        );
    }

    /// **The correctness guard: a genuinely prohibited self-sacrifice source must
    /// report `false`.** The opposite fixture from Guard 2's test — here the
    /// `CantPayCost { Sacrifice { creature } }` filter **matches** the source, so
    /// the source really cannot pay.
    ///
    /// The answer must arrive from the **readiness gate** (`is_payable_for_mana_ability`
    /// -> the `SelfRef` sacrifice arm's `!player_cant_sacrifice_as_cost` check),
    /// BEFORE the cheap-gate decision is consulted. That is why
    /// `state_clone_for_legality == 0` is load-bearing here: it pins that the
    /// `false` came from readiness and not from a simulation. It goes red if
    /// anyone reorders the decision seam ahead of the readiness gate.
    ///
    /// Both sub-cases carry a paired no-static control asserting `true`, so the
    /// negative cannot pass because of summoning sickness, a tapped bit, or a
    /// missing core type. **Deliberate asymmetry:** the control also reports 0
    /// clones (it takes the fast path), so the discriminator between the arms is
    /// `activatable`, never the counter.
    #[test]
    fn self_sacrifice_under_cant_pay_cost_static_reports_unactivatable() {
        for (label, cost) in [
            ("tapless self-sacrifice creature", self_sacrifice_cost()),
            (
                "tap-anchored self-sacrifice creature",
                tap_and_self_sacrifice_cost(),
            ),
        ] {
            let build = |with_static: bool| {
                let mut state = GameState::new_two_player(42);
                let source = spawn_mana_source(
                    &mut state,
                    9360,
                    PlayerId(0),
                    "Wild Cantor",
                    cost.clone(),
                    any_one_color(QuantityExpr::Fixed { value: 1 }),
                );
                {
                    let obj = state.objects.get_mut(&source).unwrap();
                    // CR 118.3: the prohibition's filter is applied to the
                    // sacrificed object itself, so the source must be a creature
                    // for the static to match it.
                    obj.card_types.core_types.push(CoreType::Creature);
                    // CR 302.6: keep the {T} sub-case out of the summoning-sickness
                    // gate, so the only reason for a `false` is the prohibition.
                    obj.summoning_sick = false;
                }
                if with_static {
                    install_cant_sacrifice_creature_static(&mut state, 9361, PlayerId(0));
                }
                let def = state.objects.get(&source).unwrap().abilities[0].clone();
                (state, source, def)
            };

            let (mut state, source, def) = build(true);
            flush_and_reset(&mut state);
            assert!(
                static_kind_present(&state, StaticModeKind::CantPayCost),
                "{label}: the CantPayCost static must be functioning, or this \
                 negative assertion is vacuous"
            );
            let activatable = can_activate_mana_ability_now(&state, PlayerId(0), source, 0, &def);
            let snap = crate::game::perf_counters::snapshot();

            assert!(
                !activatable,
                "{label}: a creature under a `can't sacrifice a creature to pay a \
                 cost` static can't pay its own self-sacrifice mana cost (CR 118.3)"
            );
            assert_eq!(
                snap.state_clone_for_legality, 0,
                "{label}: the `false` must come from the readiness gate, BEFORE \
                 the cheap-gate decision — not from a legality simulation"
            );

            let (mut control, source, def) = build(false);
            flush_and_reset(&mut control);
            assert!(
                !static_kind_present(&control, StaticModeKind::CantPayCost),
                "{label}: control board carries no CantPayCost static"
            );
            let activatable = can_activate_mana_ability_now(&control, PlayerId(0), source, 0, &def);
            assert!(
                activatable,
                "{label}: without the static the SAME source is activatable — this \
                 is what proves the `false` above is caused by the prohibition and \
                 not by sickness, a tapped bit, or a missing core type"
            );
        }
    }

    /// **CR 616.1 — the replacement disposition, tested rather than asserted.**
    /// A "would be put into a graveyard" `Moved` replacement applies to the
    /// sacrifice's inner battlefield -> graveyard move, so `sacrifice_permanent`
    /// returns `NeedsReplacementChoice`. The self-sacrifice payment arm maps that
    /// to `Ok(ManaAbilityPaymentProgress::Paused)`, which the payment loop returns
    /// as `Ok` — so the simulation reports `true`, the same answer the fast path
    /// reports. A replacement makes the payment **pause**, never **fail**.
    ///
    /// **Reach-guard first, and it is mandatory:** without it the test would pass
    /// on a replacement definition that never matched, which is the exact failure
    /// mode this row exists to rule out.
    #[test]
    fn self_sacrifice_with_graveyard_replacement_matches_simulation() {
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9370, PlayerId(0), ManaColor::ALL.to_vec());
        let leyline = create_object(
            &mut state,
            CardId(9371),
            PlayerId(0),
            "Leyline of the Void".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&leyline)
            .unwrap()
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            .mode(ReplacementMode::Optional { decline: None })]
        .into();
        let def = state.objects.get(&treasure).unwrap().abilities[0].clone();
        crate::game::layers::flush_layers(&mut state);

        // Reach-guard: prove on a throwaway clone that the definition genuinely
        // intercepts this sacrifice's inner graveyard move.
        let mut probe = state.clone();
        let outcome =
            sacrifice::sacrifice_permanent(&mut probe, treasure, PlayerId(0), &mut Vec::new())
                .expect("sacrificing an on-battlefield permanent must not error");
        assert!(
            matches!(
                outcome,
                sacrifice::SacrificeOutcome::NeedsReplacementChoice(_)
            ),
            "the graveyard-move replacement must really apply (CR 616.1), or the \
             equivalence below would be asserted on a replacement that never fired"
        );

        assert!(
            can_activate_mana_ability_by_simulation(&state, PlayerId(0), treasure, 0, &def),
            "a CR 616.1 replacement makes the simulated payment PAUSE, not fail — \
             `activate_mana_ability` still returns Ok"
        );
        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), treasure, 0, &def),
            "the fast path reports the same answer, so no guard is needed for the \
             replacement axis"
        );
    }

    /// CR 605.3a + CR 106.12: A ready plain `{T}: Add` source is activatable and
    /// its `{T}`-only cost is conclusively payable by the cheap gate, so NO
    /// legality clone is taken — B (revert-failing: pre-fix takes one clone here).
    #[test]
    fn plain_tap_mana_source_skips_legality_clone() {
        let mut state = GameState::new_two_player(42);
        let dork = make_tap_any_color_creature(&mut state, 9302, PlayerId(0), false);
        let def = state.objects.get(&dork).unwrap().abilities[0].clone();

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), dork, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(activatable, "a ready {{T}}: Add source is activatable");
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "a {{T}}-only mana cost is conclusively payable by the cheap gate — no clone"
        );
    }

    /// CR 601.2g: A filter land's `Composite{{Mana, Tap}}` cost still simulates —
    /// the cheap-gate skip must not apply to mana sub-costs. Affordable pool keeps
    /// it activatable (behavior preserved). C — behavior-preservation only, so we
    /// do NOT assert a zero clone count.
    #[test]
    fn filter_land_composite_still_activatable_via_simulation() {
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), ruins, 0, &ability);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "an affordable filter land remains activatable (behavior preserved)"
        );
        assert!(
            snap.state_clone_for_legality >= 1,
            "a Composite{{Mana, Tap}} cost must still simulate (CR 601.2g)"
        );
    }

    /// CR 605.3a: The board-wide mana-display sweep over N untapped `{T}: Add`
    /// sources takes ZERO legality clones — the headline regression. Pre-fix every
    /// source cloned + simulated (N clones, the Cryptolith-Rite clone-storm). D.
    #[test]
    fn mana_display_sweep_is_clone_free_for_tap_only_sources() {
        const N: usize = 8;
        let mut state = GameState::new_two_player(42);
        for i in 0..N {
            make_tap_any_color_creature(&mut state, 9400 + i as u64, PlayerId(0), false);
        }
        assert_eq!(
            state.battlefield.len(),
            N,
            "board has exactly N {{T}}: Add sources"
        );

        crate::game::public_state::mark_mana_display_dirty(&mut state);
        crate::game::perf_counters::reset();
        crate::game::derived::derive_display_state(&mut state);
        let snap = crate::game::perf_counters::snapshot();

        assert_eq!(
            snap.mana_display_sweeps, 1,
            "exactly one board-wide mana sweep"
        );
        assert_eq!(
            snap.mana_display_swept_objects, N as u64,
            "the sweep visited all N battlefield objects"
        );
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "no per-source legality clone for {{T}}-only sources (revert-failing: pre-fix = N clones)"
        );
    }

    /// Direct classifier unit tests for `AbilityCost::all_components_cheap_gate_covered`
    /// and the `cost_conclusively_payable_by_cheap_gate` wrapper anchor guard.
    #[test]
    fn cheap_gate_cost_classification_units() {
        assert!(AbilityCost::Tap.all_components_cheap_gate_covered());
        assert!(AbilityCost::Untap.all_components_cheap_gate_covered());
        assert!(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Untap]
        }
        .all_components_cheap_gate_covered());
        assert!(!AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Mana {
                    cost: ManaCost::generic(1)
                }
            ]
        }
        .all_components_cheap_gate_covered());
        assert!(!AbilityCost::Mana {
            cost: ManaCost::generic(1)
        }
        .all_components_cheap_gate_covered());
        // Empty composite is vacuously all()-true at the classifier level...
        assert!(AbilityCost::Composite { costs: vec![] }.all_components_cheap_gate_covered());

        // ...but the wrapper's {T}/{Q} anchor guards the degenerate empty
        // Composite, and a None cost is conclusively payable (no cost to pay).
        assert!(mana_sources::cost_conclusively_payable_by_cheap_gate(&None));
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &Some(AbilityCost::Composite { costs: vec![] })
        ));
        assert!(mana_sources::cost_conclusively_payable_by_cheap_gate(
            &Some(AbilityCost::Tap)
        ));
    }

    /// Hostile classifier coverage: costs whose every component is NOT a
    /// tap/untap symbol must NOT be skipped (the wrapper returns false, so the
    /// caller falls through to full simulation). Mill needs a populated library
    /// and EffectCost an arbitrary effect, so these are asserted at the
    /// classifier/wrapper level rather than as full runtime cards; the runtime
    /// "falls through to simulate" path itself is exercised by
    /// `non_self_sacrifice_mana_cost_still_simulates` (A(b)) and
    /// `filter_land_composite_still_activatable_via_simulation` (C).
    #[test]
    fn cheap_gate_hostile_costs_must_simulate() {
        let tap_mill = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Mill { count: 1 }],
        });
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &tap_mill
        ));

        let effect_cost = Some(AbilityCost::EffectCost {
            effect: Box::new(Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }),
        });
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &effect_cost
        ));

        // Bare Mana-only cost (no {T}) — the wrapper's anchor requires a {T}/{Q}
        // component, so a mana-only cost is never skipped.
        let mana_only = Some(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &mana_only
        ));

        // None cost is conclusively payable (covered above too) — sanity anchor.
        assert!(mana_sources::cost_conclusively_payable_by_cheap_gate(&None));
    }

    /// CR 302.6 / CR 702.10: A summoning-sick creature's `{T}` mana ability is not a
    /// batch sibling, but granting Haste lifts the gate so the twin batches again.
    #[test]
    fn batch_excludes_summoning_sick_tap_mana_creature() {
        let mut state = GameState::new_two_player(42);
        let ready = make_tap_any_color_creature(&mut state, 9600, PlayerId(0), false);
        let sick = make_tap_any_color_creature(&mut state, 9601, PlayerId(0), true);
        let def = state.objects.get(&ready).unwrap().abilities[0].clone();

        // CR 302.6: summoning-sick {T} mana creature is NOT a batch sibling.
        let siblings = batch_eligible_siblings(&state, PlayerId(0), ready, &def);
        assert!(
            !siblings.contains(&sick),
            "summoning-sick {{T}} mana creature must not batch (CR 302.6)"
        );

        // CR 702.10: Haste lifts the gate → the twin becomes a valid sibling.
        state
            .objects
            .get_mut(&sick)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Haste);
        let siblings = batch_eligible_siblings(&state, PlayerId(0), ready, &def);
        assert!(
            siblings.contains(&sick),
            "a hasty {{T}} mana creature IS a batch sibling (CR 702.10)"
        );
    }

    /// CR 702.26b: Phased-out permanents are treated as though they do not
    /// exist, so a phased-out mana source cannot batch with a visible twin.
    #[test]
    fn batch_excludes_phased_out_mana_sibling() {
        let mut state = GameState::new_two_player(42);
        let ready = make_any_color_treasure(&mut state, 9700, PlayerId(0), ManaColor::ALL.to_vec());
        let phased =
            make_any_color_treasure(&mut state, 9701, PlayerId(0), ManaColor::ALL.to_vec());
        let def = state.objects.get(&ready).unwrap().abilities[0].clone();

        let mut events = Vec::new();
        crate::game::phasing::phase_out_object(
            &mut state,
            phased,
            crate::game::game_object::PhaseOutCause::Directly,
            &mut events,
        );

        let siblings = batch_eligible_siblings(&state, PlayerId(0), ready, &def);
        assert!(
            !siblings.contains(&phased),
            "phased-out mana sources must not batch (CR 702.26b)"
        );
        assert!(
            !can_activate_mana_ability_now(&state, PlayerId(0), phased, 0, &def),
            "phased-out mana sources must fail the readiness gate (CR 702.26b)"
        );
        let rejected = activate_mana_ability(
            &mut state,
            phased,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        );
        assert!(
            matches!(rejected, Err(EngineError::ActionNotAllowed(_))),
            "phased-out mana sources must fail executor activation (CR 702.26b)"
        );
    }

    /// CR 605.3a: A count larger than the available sources is rejected before
    /// any mana is produced — no partial application.
    #[test]
    fn batch_activation_rejects_count_above_available() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9101, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9102, PlayerId(0), ManaColor::ALL.to_vec());

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("activate first Treasure");

        let rejected = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 5,
            },
        );
        assert!(
            rejected.is_err(),
            "count 5 with only two sources is illegal"
        );
        assert!(
            state
                .objects
                .get(&b)
                .is_some_and(|o| o.zone == Zone::Battlefield),
            "the sibling is untouched by the rejected batch"
        );
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "no mana is produced when the batch is rejected"
        );
    }

    /// CR 605.3b: The default `count = 1` resolves a single source — twins are
    /// left untouched (back-compatible single-tap behavior).
    #[test]
    fn batch_activation_default_count_resolves_single_source() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9151, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9152, PlayerId(0), ManaColor::ALL.to_vec());

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("activate first Treasure");
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
        )
        .expect("single color choice resolves");

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert!(
            state
                .objects
                .get(&b)
                .is_some_and(|o| o.zone == Zone::Battlefield),
            "the sibling remains untapped on the battlefield"
        );
    }

    /// CR 605.3a: Only definition-identical twins batch together — a different
    /// any-color source (distinct ability) is excluded.
    #[test]
    fn batch_groups_only_identical_ability_definitions() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9201, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9202, PlayerId(0), ManaColor::ALL.to_vec());
        // Distinct AbilityDefinition (only W/U) → not a twin of the 5-color pair.
        let _other = make_any_color_treasure(
            &mut state,
            9203,
            PlayerId(0),
            vec![ManaColor::White, ManaColor::Blue],
        );

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("activate the 5-color Treasure");

        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &result.waiting_for
        else {
            panic!("expected ChooseManaColor, got {:?}", result.waiting_for);
        };
        assert_eq!(
            pending.batch_siblings,
            vec![b],
            "only the identical 5-color Treasure is offered as a twin"
        );
    }

    /// CR 605.3a: `cost_resolves_without_choice` is the batch eligibility gate —
    /// a deny-by-default whitelist of `Tap`, self-sacrifice, and `Composite`s of
    /// those. Any choice-bearing or unrecognized cost is excluded.
    #[test]
    fn cost_resolves_without_choice_whitelist() {
        // Treasure: Tap + self-sacrifice → batchable.
        assert!(cost_resolves_without_choice(&Some(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }
        )));
        assert!(cost_resolves_without_choice(&Some(AbilityCost::Tap)));
        assert!(cost_resolves_without_choice(&None));

        // Phyrexian Altar: sacrifice a (non-self) creature → requires a choice.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::creature()),
                1
            ))
        )));
        // Self-sacrifice of more than one is not the single-token shape.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 2))
        )));
        // Filter-land style mana sub-cost requires a payment choice.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 1,
                        },
                    },
                    AbilityCost::Tap,
                ],
            }
        )));
        // Pay-life is non-interactive but conservatively excluded (deny-by-default).
        assert!(!cost_resolves_without_choice(&Some(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        })));
    }

    #[test]
    fn resolve_composite_cost_taps_pays_life_and_produces_mana() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::White, ManaColor::Blue],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Blue)),
        )
        .unwrap();

        assert!(state.objects.get(&obj_id).unwrap().tapped);
        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::LifeChanged {
                player_id,
                amount: -1,
            } if *player_id == PlayerId(0)
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn lions_eye_diamond_discards_hand_and_then_produces_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let led = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Lion's Eye Diamond".to_string(),
            Zone::Battlefield,
        );
        let c1 = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Card One".to_string(),
            Zone::Hand,
        );
        let c2 = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Card Two".to_string(),
            Zone::Hand,
        );

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 3 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Discard {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&led).unwrap().abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            led,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let pending = match waiting {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::Discard,
                count,
                choices: cards,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(cards.len(), 2);
                *pending_mana_ability
            }
            other => panic!("expected PayCost Discard (mana ability), got {other:?}"),
        };

        let waiting = handle_discard_for_mana_ability(
            &mut state,
            2,
            &[c1, c2],
            &pending,
            &[c1, c2],
            &mut events,
        )
        .unwrap();

        let pending = match waiting {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                context,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(options.len(), 5);
                *expect_mana_ability_context(context)
            }
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };

        assert!(!state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
        assert!(state.players[0].graveyard.contains(&c1));
        assert!(state.players[0].graveyard.contains(&c2));
        assert_ne!(
            state.objects.get(&led).map(|obj| obj.zone),
            Some(Zone::Battlefield)
        );

        handle_choose_mana_color(
            &mut state,
            &pending,
            &ManaChoicePrompt::SingleColor {
                options: vec![
                    ManaType::White,
                    ManaType::Blue,
                    ManaType::Black,
                    ManaType::Red,
                    ManaType::Green,
                ],
            },
            ManaChoice::SingleColor(ManaType::Red),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 3);
    }

    /// CR 605.1a + CR 106.1: Build a Lion's-Eye-Diamond-shaped mana ability —
    /// `Composite[Discard { HandSize }, Sacrifice(SelfRef)]` producing three mana
    /// of one chosen color. Mirrors the real card's parsed cost
    /// (`Discard { count: Ref HandSize(Controller), self_scope: FromHand }` +
    /// `Sacrifice(SelfRef, 1)`), so a name change alone re-targets it to any card
    /// in the same "discard your hand, sacrifice ~: add three mana" class.
    fn discard_hand_sacrifice_three_mana_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 3 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Discard {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        })
    }

    /// Issue #6494 (PRIMARY revert-guard, mana path): Lion's Eye Diamond with an
    /// EMPTY hand must be activatable — its "Discard your hand" leg with an empty
    /// hand is a zero-card discard, paid by doing nothing (CR 601.2h + CR 701.9a).
    ///
    /// Oracle (Scryfall-verified): "Discard your hand, Sacrifice this artifact:
    /// Add three mana of any one color. Activate only as an instant."
    ///
    /// At base the activation surfaces `WaitingFor::PayCost { Discard, count: 0 }`
    /// (a dead prompt that re-emits forever), so the `ChooseManaColor` assertion
    /// fails with `left: PayCost right: ChooseManaColor` — revert-sensitive to the
    /// zero-count auto-pay guard `discard_cost_choice` now inherits from the shared
    /// `casting::resolve_non_self_discard_requirement` authority. The 3-mana
    /// assertion is a positive reach-guard (proves production, not a vacuous halt);
    /// the sacrifice assertion proves the guard consumes ONLY the discard leg and
    /// the self-sacrifice still fires.
    #[test]
    fn lions_eye_diamond_activates_empty_handed_and_produces_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let led = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Lion's Eye Diamond".to_string(),
            Zone::Battlefield,
        );
        // No cards in hand — HandSize(Controller) resolves to 0.
        assert!(state.players[0].hand.is_empty());

        let ability = discard_hand_sacrifice_three_mana_ability();
        Arc::make_mut(&mut state.objects.get_mut(&led).unwrap().abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            led,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        // CR 601.2h + CR 701.9a: no dead PayCost { Discard, count: 0 } — the
        // activation advances straight to the color choice.
        let pending = match waiting {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                context,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(options.len(), 5);
                *expect_mana_ability_context(context)
            }
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };

        // CR 701.9a: nothing was discarded — the hand stays empty.
        assert!(state.players[0].hand.is_empty());
        // The self-sacrifice leg still fired — LED left the battlefield for the
        // graveyard, and it is the ONLY card there (the zero-card discard added
        // nothing), proving the guard consumed only the discard leg.
        assert_eq!(state.players[0].graveyard.len(), 1);
        assert!(state.players[0].graveyard.contains(&led));
        assert_ne!(
            state.objects.get(&led).map(|obj| obj.zone),
            Some(Zone::Battlefield)
        );

        handle_choose_mana_color(
            &mut state,
            &pending,
            &ManaChoicePrompt::SingleColor {
                options: vec![
                    ManaType::White,
                    ManaType::Blue,
                    ManaType::Black,
                    ManaType::Red,
                    ManaType::Green,
                ],
            },
            ManaChoice::SingleColor(ManaType::Red),
            &mut events,
        )
        .unwrap();

        // Positive reach-guard: production actually ran (not a vacuous halt).
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 3);
    }

    /// Issue #6494 (mana path, class not card): Diamond Lion shares Lion's Eye
    /// Diamond's "Discard your hand, Sacrifice ~: Add three mana of any one color"
    /// shape, so the empty-hand fix is class-wide, not keyed to one card. Only the
    /// object name differs from the LED case above.
    #[test]
    fn diamond_lion_activates_empty_handed_and_produces_chosen_color() {
        let mut state = GameState::new_two_player(7);
        let lion = create_object(
            &mut state,
            CardId(40),
            PlayerId(0),
            "Diamond Lion".to_string(),
            Zone::Battlefield,
        );
        assert!(state.players[0].hand.is_empty());

        let ability = discard_hand_sacrifice_three_mana_ability();
        Arc::make_mut(&mut state.objects.get_mut(&lion).unwrap().abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            lion,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let pending = match waiting {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { .. },
                context,
                ..
            } => *expect_mana_ability_context(context),
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };
        assert_ne!(
            state.objects.get(&lion).map(|obj| obj.zone),
            Some(Zone::Battlefield)
        );

        handle_choose_mana_color(
            &mut state,
            &pending,
            &ManaChoicePrompt::SingleColor {
                options: vec![
                    ManaType::White,
                    ManaType::Blue,
                    ManaType::Black,
                    ManaType::Red,
                    ManaType::Green,
                ],
            },
            ManaChoice::SingleColor(ManaType::Green),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
    }

    /// Helper: build a Pit-of-Offerings-style permanent with a `{T}: Add one mana
    /// of any of the exiled cards' colors` mana ability and exile a card linked
    /// to it via `state.exile_links` (the same relation populated by the
    /// `ChangeZone` resolver during the ETB trigger).
    fn pit_of_offerings_with_exiled_card(
        state: &mut GameState,
        owner: PlayerId,
        exiled_card_name: &str,
        exiled_colors: Vec<ManaColor>,
    ) -> (ObjectId, ObjectId) {
        let pit = create_object(
            state,
            CardId(1000),
            owner,
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pit).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.has_mana_ability = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChoiceAmongExiledColors {
                            source: LinkedExileScope::ThisObject,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
        let exiled = create_object(
            state,
            CardId(2000),
            owner,
            exiled_card_name.to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled).unwrap().color = exiled_colors;
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: pit,
            kind: ExileLinkKind::TrackedBySource,
        });
        (pit, exiled)
    }

    #[test]
    fn pit_of_offerings_with_no_exiled_colored_cards_produces_no_mana() {
        // CR 605.1a + CR 106.5: With zero linked colored exiles the ability has
        // no defined mana type — produces no mana even though the tap cost is
        // paid (the ability is still legal to activate per CR 605.3a).
        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pit).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChoiceAmongExiledColors {
                            source: LinkedExileScope::ThisObject,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, pit, PlayerId(0), &def, &mut events, None).unwrap();

        assert!(state.objects.get(&pit).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.total(), 0);
        // can_activate_mana_ability_now confirms it's still legal — paying the
        // tap is a valid resolution even when no mana is produced.
    }

    #[test]
    fn pit_of_offerings_colorless_exiled_card_produces_no_mana() {
        // CR 106.5: A Mountain card itself has no `colors` (red is implied via
        // its mana ability, not by intrinsic color). For Pit of Offerings the
        // relevant property is the exiled card's printed colors; a card with
        // no printed colors contributes nothing.
        let mut state = GameState::new_two_player(42);
        let (pit, _exiled) =
            pit_of_offerings_with_exiled_card(&mut state, PlayerId(0), "Mountain", vec![]);

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, pit, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn pit_of_offerings_with_one_colored_exile_produces_that_color() {
        // Single colored exile (Island = Blue): the only legal mana type is {U}.
        let mut state = GameState::new_two_player(42);
        let (pit, _) = pit_of_offerings_with_exiled_card(
            &mut state,
            PlayerId(0),
            "Savannah Lions",
            vec![ManaColor::White],
        );

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, pit, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn pit_of_offerings_color_options_excludes_colorless_exiles() {
        // CR 605.1a + CR 106.5: With a colorless `Mountain` and a blue `Island`
        // exiled, only `{U}` is a legal mana option.
        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pit)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&pit).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let mountain = create_object(
            &mut state,
            CardId(2001),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Exile,
        );
        // Mountain's intrinsic `color` is empty (its red identity comes from its
        // mana ability, not its colors field).
        state.objects.get_mut(&mountain).unwrap().color = vec![];
        let island = create_object(
            &mut state,
            CardId(2002),
            PlayerId(0),
            "Island".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&island).unwrap().color = vec![];
        let counterspell = create_object(
            &mut state,
            CardId(2003),
            PlayerId(0),
            "Counterspell".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&counterspell).unwrap().color = vec![ManaColor::Blue];

        for exiled in [mountain, island, counterspell] {
            state.exile_links.push(ExileLink {
                exiled_id: exiled,
                source_id: pit,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        // Direct query of the option set: only blue should be legal.
        let options = crate::game::effects::mana::exiled_color_options(
            &state,
            LinkedExileScope::ThisObject,
            pit,
        );
        assert_eq!(options, vec![ManaType::Blue]);
    }

    #[test]
    fn pit_of_offerings_color_override_picks_chosen_color() {
        // Two colored exiles → two legal mana types. With a `color_override`,
        // the ability produces exactly that color (mirrors AnyOneColor).
        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pit)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&pit).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let white_card = create_object(
            &mut state,
            CardId(2001),
            PlayerId(0),
            "White Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&white_card).unwrap().color = vec![ManaColor::White];
        let blue_card = create_object(
            &mut state,
            CardId(2002),
            PlayerId(0),
            "Blue Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&blue_card).unwrap().color = vec![ManaColor::Blue];

        for exiled in [white_card, blue_card] {
            state.exile_links.push(ExileLink {
                exiled_id: exiled,
                source_id: pit,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            pit,
            PlayerId(0),
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Blue)),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn pit_of_offerings_etb_exile_populates_links_then_mana_ability_consumes_them() {
        // End-to-end: drive the ETB-style exile through the actual `change_zone`
        // resolver so `state.exile_links` is auto-populated by the engine
        // (mirrors how Pit of Offerings' "When this land enters, exile up to
        // three target cards from graveyards" trigger resolves), then activate
        // the colored mana ability and confirm it produces a color drawn from
        // the just-exiled cards.
        use crate::types::ability::{Effect as Ef, ResolvedAbility, TargetFilter, TargetRef};

        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pit)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&pit).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Ef::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // Place a single colored creature card in the graveyard for Pit's ETB
        // trigger to exile via `ChangeZone`.
        let lions = create_object(
            &mut state,
            CardId(2001),
            PlayerId(0),
            "Savannah Lions".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&lions).unwrap().color = vec![ManaColor::White];

        // Resolve Pit's ETB exile through the real `change_zone` resolver. This
        // is the same path the trigger system uses; a successful Exile move
        // should automatically push an `ExileLink::TrackedBySource` into
        // `state.exile_links` (see `change_zone::execute_zone_move`).
        let etb = ResolvedAbility::new(
            Ef::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(lions)],
            pit,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::change_zone::resolve(&mut state, &etb, &mut events).unwrap();

        // Sanity: the ETB resolver populated the link.
        assert!(
            state
                .exile_links
                .iter()
                .any(|link| link.source_id == pit && link.exiled_id == lions),
            "ETB-style exile must populate state.exile_links via the standard \
             change_zone resolver (CR 610.3)"
        );

        // Now activate the colored mana ability. With one white-colored exiled
        // card, the only legal mana type is `{W}`.
        let mana_def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut mana_events = Vec::new();
        resolve_mana_ability(
            &mut state,
            pit,
            PlayerId(0),
            &mana_def,
            &mut mana_events,
            None,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
    }

    #[test]
    fn pit_of_offerings_blink_clears_exile_links() {
        // CR 400.7: A blink (LTB then re-ETB) creates a new object that inherits
        // no linkage — the re-entered Pit's mana ability must not read the old
        // incarnation's exiled cards.
        //
        // CR 607.2a: `TrackedBySource` links now SURVIVE the leg into exile (a
        // self-exiled source stays the linked-ability referent for its pile —
        // Mechtitan Core); the blink reset happens on RE-ENTRY, not on the exit.
        // While Pit sits in exile the preserved link is inert (its "{T}: add mana
        // among cards exiled with ~" ability can only be activated on the
        // battlefield), so this staged blink still ends with no stale linkage.
        let mut state = GameState::new_two_player(42);
        let (pit, _exiled) = pit_of_offerings_with_exiled_card(
            &mut state,
            PlayerId(0),
            "Llanowar Elves",
            vec![ManaColor::Green],
        );

        assert_eq!(state.exile_links.len(), 1, "precondition: link was created");

        // LTB into exile: the link is preserved (CR 607.2a durability), not yet cleared.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, pit, Zone::Exile, &mut events);
        assert!(
            state.exile_links.iter().any(|link| link.source_id == pit),
            "an exit into exile preserves the TrackedBySource link (CR 607.2a)"
        );

        // Re-ETB (the blink completes): the new object sheds the stale linkage.
        crate::game::zones::move_to_zone(&mut state, pit, Zone::Battlefield, &mut events);
        assert!(
            state.exile_links.iter().all(|link| link.source_id != pit),
            "TrackedBySource exile links must be cleared when the source re-enters \
             the battlefield as a new object (CR 400.7)"
        );
    }

    #[test]
    fn color_override_produces_specified_color() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Any Color Source".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::White, ManaColor::Blue, ManaColor::Black],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        // Override to produce Black specifically
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Black)),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    // ─────────────────────────────────────────────────────────────
    // is_triggered_mana_ability — CR 605.1b classifier edge cases.
    // ─────────────────────────────────────────────────────────────

    fn mana_producing_resolved() -> ResolvedAbility {
        ResolvedAbility::new(
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
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn draw_resolved() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn tapped_for_mana_event() -> GameEvent {
        GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
            produced: vec![ManaType::Green],
            tap_state: crate::types::events::ManaTapState::FromTap,
        }
    }

    #[test]
    fn classifier_accepts_head_effect_mana_on_tapped_for_mana() {
        let ability = mana_producing_resolved();
        assert!(is_triggered_mana_ability(
            &ability,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_non_tapped_for_mana_event() {
        // CR 605.1b criterion (b) + CR 106.12a: only a `TappedForMana` event
        // (a `{T}`-cost mana ability resolving) qualifies. An unrelated event
        // (e.g. `AbilityActivated`) must not route through the inline resolver.
        let ability = mana_producing_resolved();
        let ev = GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
            kind: crate::types::events::ActivatedAbilityKind::Normal,
        };
        assert!(!is_triggered_mana_ability(&ability, Some(&ev)));
    }

    #[test]
    fn classifier_accepts_all_mana_chain() {
        // CR 605.1b criterion (c): every reachable link must be mana. A chain
        // with head + sub both producing mana (e.g., "add G, then add G") is
        // inline-safe.
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(mana_producing_resolved()));
        assert!(is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_mixed_mana_plus_non_mana_chain() {
        // CR 605.1b criterion (c): "every link is mana" — a chain with mana
        // at the head but a non-mana sub (e.g., draw a card) MUST use the
        // stack. Routing such a chain inline would silently perform the
        // non-mana effect without giving players priority.
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_chain_without_any_mana_effect() {
        let mut head = draw_resolved();
        head.sub_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_sub_ability_with_multi_target() {
        // CR 605.1b criterion (a) + CR 115.6: any link declaring targets
        // anywhere in the chain disqualifies inline resolution.
        let mut sub = mana_producing_resolved();
        sub.multi_target = Some(MultiTargetSpec::fixed(1, 1));
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(sub));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_sub_ability_with_resolved_targets() {
        // Symmetric to multi_target: a non-empty `targets` vec (as produced
        // by auto_select_targets_for_ability at trigger time) on any link
        // also disqualifies. Covers the `|| multi_target.is_some()` branch
        // separately from the `!targets.is_empty()` branch.
        let mut sub = mana_producing_resolved();
        sub.targets = vec![crate::types::ability::TargetRef::Object(ObjectId(99))];
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(sub));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_walks_else_ability_for_criterion_c() {
        // CR 608.2c: `else_ability` is the "Otherwise" branch of a
        // conditional ability. A mana head with a non-mana `else_ability`
        // (e.g. "if X, add G; otherwise draw a card") must still use the
        // stack — inline resolution of the else branch would skip priority
        // on the draw.
        let mut head = mana_producing_resolved();
        head.else_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_walks_else_ability_for_criterion_a() {
        // Mirror for criterion (a): a targeted `else_ability` branch
        // disqualifies even when the main chain is target-free.
        let mut else_branch = mana_producing_resolved();
        else_branch.targets = vec![crate::types::ability::TargetRef::Object(ObjectId(7))];
        let mut head = mana_producing_resolved();
        head.else_ability = Some(Box::new(else_branch));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn inline_triggered_mana_ability_resolves_trigger_event_mana_type() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::TriggerEventManaType,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(77),
            PlayerId(0),
        );
        let event = GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
            produced: vec![ManaType::Red],
            tap_state: crate::types::events::ManaTapState::FromTap,
        };
        let mut events = Vec::new();

        resolve_triggered_mana_ability_inline(
            &mut state,
            &ability,
            Some(&event),
            &mut events,
            None,
        );

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.current_trigger_event.is_none());
    }

    #[test]
    fn taps_for_mana_trigger_adds_trigger_event_mana_to_triggering_player() {
        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Mountain".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let mana_flare = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mana Flare".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&mana_flare)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(TypedFilter::land())),
            );

        crate::game::triggers::process_triggers(
            &mut state,
            &[GameEvent::TappedForMana {
                player_id: PlayerId(1),
                source_id: land,
                produced: vec![ManaType::Red],
                tap_state: crate::types::events::ManaTapState::FromTap,
            }],
        );

        assert_eq!(state.players[0].mana_pool.total(), 0);
        assert_eq!(state.players[1].mana_pool.count_color(ManaType::Red), 1);
    }

    #[test]
    fn taps_for_mana_cant_untap_trigger_binds_triggering_land() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.tapped = true;
        }

        let vorinclex = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        let duration = Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: PlayerScope::Controller,
        };
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::GenericEffect {
                                static_abilities: vec![StaticDefinition::new(
                                    StaticMode::CantUntap,
                                )
                                .affected(TargetFilter::ParentTarget)
                                .modifications(vec![ContinuousModification::AddStaticMode {
                                    mode: StaticMode::CantUntap,
                                }])],
                                duration: Some(duration.clone()),
                                target: Some(TargetFilter::TriggeringSource),
                                end_cost: None,
                            },
                        )
                        .duration(duration),
                    )
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::Opponent),
                    )),
            );

        crate::game::triggers::process_triggers(
            &mut state,
            &[GameEvent::TappedForMana {
                player_id: PlayerId(1),
                source_id: land,
                produced: vec![ManaType::Green],
                tap_state: crate::types::events::ManaTapState::FromTap,
            }],
        );
        assert_eq!(state.stack.len(), 1);

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(state.transient_continuous_effects.iter().any(|effect| {
            effect.affected == (TargetFilter::SpecificObject { id: land })
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    })
        }));

        crate::game::turns::execute_untap(&mut state, &mut events);
        assert!(state.objects[&land].tapped);
        assert!(state.transient_continuous_effects.is_empty());
    }

    #[test]
    fn activate_any_one_color_pauses_for_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        state.turn_number = 3;

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        match &result {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(options, &[ManaType::Red, ManaType::Green]);
            }
            _ => panic!("expected ChooseManaColor::SingleColor, got {:?}", result),
        }
    }

    #[test]
    fn handle_choose_mana_color_produces_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut obj.abilities).push(ability);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::SingleColor {
            options: vec![ManaType::Red, ManaType::Green],
        };
        let mut events = Vec::new();

        let result = handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::SingleColor(ManaType::Green),
            &mut events,
        )
        .unwrap();

        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "should resume to Priority"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            1,
            "should have 1 green mana"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            0,
            "should have 0 red mana"
        );
    }

    // --- Foraging Wickermaw: "Add one mana of any color. This creature becomes
    // that color until end of turn." (parser arm + mana-producer color record) ---

    /// Parse Foraging Wickermaw's verbatim activated line into its single mana
    /// ability. The `{1}` cost, `AnyOneColor` head, `becomes that color` sub-chain
    /// (now `AddChosenColor`), UEOT duration, and once-each-turn restriction all
    /// come from the real parser pipeline.
    fn foraging_wickermaw_mana_ability() -> AbilityDefinition {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "{1}: Add one mana of any color. This creature becomes that color until end of turn. Activate only once each turn.",
            "Foraging Wickermaw",
            &[],
            &["Creature".to_string()],
            &[],
        );
        assert_eq!(
            parsed.abilities.len(),
            1,
            "expected exactly one activated mana ability"
        );
        parsed.abilities.into_iter().next().unwrap()
    }

    fn foraging_wickermaw_setup() -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Foraging Wickermaw".to_string(),
            Zone::Battlefield,
        );
        let ability = foraging_wickermaw_mana_ability();
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(ability);
        (state, source)
    }

    fn wubrg_single_color_prompt() -> ManaChoicePrompt {
        ManaChoicePrompt::SingleColor {
            options: vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ],
        }
    }

    fn pending_for(source: ObjectId) -> PendingManaAbility {
        PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_snapshot: None,
            ability_index: Some(0),
            rules_execution_node: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        }
    }

    /// Drive the real color-choice completion handler (the entry point the engine
    /// dispatches a `ChooseManaColor` answer to), then recompute layers.
    fn activate_and_choose(state: &mut GameState, source: ObjectId, color: ManaType) {
        let pending = pending_for(source);
        let mut events = Vec::new();
        handle_choose_mana_color(
            state,
            &pending,
            &wubrg_single_color_prompt(),
            ManaChoice::SingleColor(color),
            &mut events,
        )
        .unwrap();
        crate::game::layers::mark_layers_full(state);
        crate::game::layers::flush_layers(state);
    }

    /// The load-bearing discriminating test: the creature's color is a FUNCTION of
    /// the mana choice (Red -> Red, Blue -> Blue), impossible to pass with any
    /// baked/first-color constant. Reverting EITHER half fails:
    ///  - parser arm removed -> become stays `Effect::Unimplemented` -> no color change;
    ///  - mana-producer record removed -> `AddChosenColor` reads `chosen_color()==None`.
    #[test]
    fn foraging_wickermaw_becomes_the_produced_color() {
        for (chosen, expected) in [
            (ManaType::Red, ManaColor::Red),
            (ManaType::Blue, ManaColor::Blue),
        ] {
            let (mut state, source) = foraging_wickermaw_setup();
            activate_and_choose(&mut state, source, chosen);
            assert_eq!(
                state.objects[&source].color,
                vec![expected],
                "creature must become the color of the mana it produced ({chosen:?})"
            );
            assert_eq!(
                state.players[0].mana_pool.count_color(chosen),
                1,
                "the produced mana also lands in the pool"
            );
        }
    }

    /// CR 400.7: `chosen_attributes` persist on the permanent across turns (cleared
    /// only on zone change). A later activation must OVERWRITE the stored `Color`,
    /// not accumulate — `chosen_color()` is first-match. Reverting the retain-drop
    /// to a plain push leaves `[Color(Red), Color(Green)]` and reads stale Red.
    #[test]
    fn foraging_wickermaw_reactivation_replaces_color_not_accumulates() {
        let (mut state, source) = foraging_wickermaw_setup();

        // Turn N: choose Red.
        activate_and_choose(&mut state, source, ManaType::Red);
        assert_eq!(state.objects[&source].color, vec![ManaColor::Red]);

        // Cross the turn boundary: the turn-N UEOT color effect expires, but the
        // stored `ChosenAttribute::Color(Red)` persists on the permanent (CR 400.7).
        crate::game::layers::prune_end_of_turn_effects(&mut state);
        state.turn_number += 1;
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);

        // Turn N+1: choose Green.
        activate_and_choose(&mut state, source, ManaType::Green);

        assert_eq!(
            state.objects[&source].color,
            vec![ManaColor::Green],
            "re-activation replaces the stored color (not stale Red, not [Red, Green])"
        );
        let color_attrs = state.objects[&source]
            .chosen_attributes
            .iter()
            .filter(|a| matches!(a, ChosenAttribute::Color(_)))
            .count();
        assert_eq!(
            color_attrs, 1,
            "exactly one stored Color attribute after re-activation"
        );
    }

    /// Gate airtightness: a bare `Add one mana of any color` ability with NO
    /// `becomes that color` clause must NOT record `ChosenAttribute::Color` — zero
    /// blast radius for basics / City of Brass / painlands / filter lands.
    /// Reverting the gate (unconditional write) makes this producer gain a spurious
    /// `Color` attribute. The pool assertion is a positive reach-guard proving the
    /// write site was reached and merely gated, not skipped upstream.
    #[test]
    fn plain_any_color_producer_records_no_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "City of Brass".to_string(),
            Zone::Battlefield,
        );
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability);

        let pending = pending_for(source);
        let mut events = Vec::new();
        handle_choose_mana_color(
            &mut state,
            &pending,
            &wubrg_single_color_prompt(),
            ManaChoice::SingleColor(ManaType::Red),
            &mut events,
        )
        .unwrap();

        assert!(
            state.objects[&source]
                .chosen_attributes
                .iter()
                .all(|a| !matches!(a, ChosenAttribute::Color(_))),
            "a plain any-color producer must not record a chosen color (gate must suppress the write)"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            1,
            "reach-guard: the ability really did produce mana"
        );
    }

    /// Parser-shape (reach-guard): the verbatim become clause lowers to a
    /// `GenericEffect` carrying `AddChosenColor` with UEOT duration, and NOTHING in
    /// the chain is `Effect::Unimplemented` (proving it parsed past the old
    /// fallthrough, not vacuously).
    #[test]
    fn foraging_wickermaw_become_clause_parses_to_add_chosen_color() {
        let ability = foraging_wickermaw_mana_ability();
        assert!(
            matches!(&*ability.effect, Effect::Mana { .. }),
            "head is the mana production"
        );
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("become sub-ability present");
        match &*sub.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert!(
                    static_abilities.iter().any(|s| s
                        .modifications
                        .iter()
                        .any(|m| matches!(m, ContinuousModification::AddChosenColor { .. }))),
                    "become clause maps to AddChosenColor"
                );
                assert!(
                    *duration == Some(Duration::UntilEndOfTurn)
                        || sub.duration == Some(Duration::UntilEndOfTurn),
                    "color change lasts until end of turn"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
        assert!(
            !matches!(&*sub.effect, Effect::Unimplemented { .. }),
            "become did not fall through to Unimplemented"
        );
        assert!(!matches!(&*ability.effect, Effect::Unimplemented { .. }));
    }

    #[test]
    fn handle_choose_mana_color_resolves_pain_land_damage_for_each_color() {
        for chosen in [ManaType::Green, ManaType::White] {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(77),
                PlayerId(0),
                "Brushland".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());

            let pending = PendingManaAbility {
                player: PlayerId(0),
                source_id: source,
                ability_index: Some(0),
                rules_execution_node: None,
                ability_snapshot: None,
                color_override: None,
                resume: ManaAbilityResume::Priority,
                cost_move_resume: None,
                chosen_tappers: Vec::new(),
                chosen_discards: Vec::new(),
                chosen_mana_payment: None,
                chosen_counter_count: None,
                chosen_x: None,
                collected_evidence: Vec::new(),
                chosen_exiled: Vec::new(),
                chosen_sacrificed_battlefield: Vec::new(),
                cost_paid_object: None,
                batch_siblings: Vec::new(),
            };
            let prompt = ManaChoicePrompt::SingleColor {
                options: vec![ManaType::Green, ManaType::White],
            };
            let mut events = Vec::new();

            let result = handle_choose_mana_color(
                &mut state,
                &pending,
                &prompt,
                ManaChoice::SingleColor(chosen),
                &mut events,
            )
            .unwrap();

            assert!(matches!(result, WaitingFor::Priority { .. }));
            assert_eq!(state.players[0].mana_pool.count_color(chosen), 1);
            assert_eq!(state.players[0].life, 19);
        }
    }

    #[test]
    fn color_override_bypasses_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        state.turn_number = 3;

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .unwrap();

        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "auto-tap with color_override should resolve immediately"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn color_override_pain_land_still_deals_damage() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(78),
            PlayerId(0),
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let ability = brushland_colored_ability();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[0].life, 19);
    }

    // ─────────────────────────────────────────────────────────────
    // ChoiceAmongCombinations (filter lands — Shadowmoor/Eventide).
    // ─────────────────────────────────────────────────────────────

    fn sunken_ruins_colored_ability() -> AbilityDefinition {
        // CR 605.3b + CR 106.1a: `{U/B}, {T}: Add {U}{U}, {U}{B}, or {B}{B}`.
        // The real printed cost is composite: one hybrid `{U/B}` plus `{T}`.
        // Tests must use the real shape — truncating to `AbilityCost::Tap`
        // masks the Composite + Mana sub-cost bug path.
        use crate::types::mana::{ManaCost, ManaCostShard};
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::ChoiceAmongCombinations {
                    options: vec![
                        vec![ManaColor::Blue, ManaColor::Blue],
                        vec![ManaColor::Blue, ManaColor::Black],
                        vec![ManaColor::Black, ManaColor::Black],
                    ],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::BlueBlack],
                        generic: 0,
                    },
                },
                AbilityCost::Tap,
            ],
        })
    }

    #[test]
    fn activate_filter_land_prompts_with_combination_options() {
        // CR 605.3b: Manual activation of a filter land (no override) must
        // surface a Combination prompt, not a SingleColor prompt.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let ability = sunken_ruins_colored_ability();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        // Seed the pool with one {U} so the `{U/B}` sub-cost has a single
        // unambiguous plan — this test focuses on the output Combination
        // prompt, not the input mana-payment prompt.
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        match &result {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::Combination { options },
                ..
            } => {
                assert_eq!(
                    options,
                    &vec![
                        vec![ManaType::Blue, ManaType::Blue],
                        vec![ManaType::Blue, ManaType::Black],
                        vec![ManaType::Black, ManaType::Black],
                    ]
                );
            }
            _ => panic!("expected ChooseManaColor::Combination, got {:?}", result),
        }
        // CR 605.3b: tap cost is paid before the prompt.
        assert!(state.objects.get(&ruins).unwrap().tapped);
        // CR 601.2h + CR 107.4e: {U/B} sub-cost was debited from the seeded pool — only
        // the two combination-produced units remain.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn handle_choose_combination_produces_exact_sequence() {
        // CR 605.3b: The chosen combination lands verbatim in the pool.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(sunken_ruins_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::Combination {
            options: vec![
                vec![ManaType::Blue, ManaType::Blue],
                vec![ManaType::Blue, ManaType::Black],
                vec![ManaType::Black, ManaType::Black],
            ],
        };
        let mut events = Vec::new();

        handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::Combination(vec![ManaType::Blue, ManaType::Black]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn combination_override_bypasses_choice_and_produces_exact_mana() {
        // Auto-tap path: override short-circuits the prompt and emits the
        // combination atomically.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let ability = sunken_ruins_colored_ability();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        // Seed one {B} so the {U/B} sub-cost is unambiguously payable; the
        // auto-tap path then short-circuits both mana-payment and
        // combination-choice prompts.
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::Combination(vec![
                ManaType::Blue,
                ManaType::Black,
            ])),
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        // Pool starts with 1 {B}; {U/B} sub-cost debits that {B}; production
        // adds 1 {U} + 1 {B} per the override.
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn handle_choose_rejects_mismatched_choice_shape() {
        // A SingleColor answer to a Combination prompt must error out.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(sunken_ruins_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::Combination {
            options: vec![
                vec![ManaType::Blue, ManaType::Blue],
                vec![ManaType::Blue, ManaType::Black],
                vec![ManaType::Black, ManaType::Black],
            ],
        };
        let mut events = Vec::new();
        let result = handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::SingleColor(ManaType::Blue),
            &mut events,
        );
        assert!(result.is_err(), "mismatched shape must be rejected");
    }

    // ─────────────────────────────────────────────────────────────
    // Filter-land mana sub-cost regression tests.
    // CR 605.3a + CR 601.2h + CR 107.4e.
    // ─────────────────────────────────────────────────────────────

    fn setup_sunken_ruins(state: &mut GameState) -> (ObjectId, AbilityDefinition) {
        let ruins = create_object(
            state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let ability = sunken_ruins_colored_ability();
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        (ruins, ability)
    }

    #[test]
    fn filter_land_auto_pays_unambiguous_mana_sub_cost() {
        // CR 605.3a + CR 107.4e: Pool has only {U}; the single legal plan
        // auto-pays without surfacing `PayManaAbilityMana`. The flow then
        // lands on `ChooseManaColor` for the combination output.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(
            matches!(
                result,
                WaitingFor::ChooseManaColor {
                    choice: ManaChoicePrompt::Combination { .. },
                    ..
                }
            ),
            "expected ChooseManaColor after unambiguous mana-sub-cost auto-pay, got {:?}",
            result,
        );
        // Pool had 1 {U}; sub-cost debited it.
        assert_eq!(state.players[0].mana_pool.total(), 0);
        // Tap component also paid.
        assert!(state.objects.get(&ruins).unwrap().tapped);
    }

    #[test]
    fn filter_land_taps_another_source_when_matching_pool_mana_is_spell_only() {
        // CR 106.6 + CR 605.3a: Spell-only {U} already in the pool cannot pay
        // this mana ability's {U/B} activation cost. The engine must ignore it
        // during hybrid-plan discovery and retain the nested mana-ability path,
        // which taps the Island for an eligible {U}.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        let island = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&island).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
        }
        seed_pool_with_restriction(
            &mut state,
            PlayerId(0),
            ManaType::Blue,
            ManaRestriction::OnlyForSpell,
        );

        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            ruins,
            0,
            &ability,
        ));

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::Combination { .. },
                ..
            }
        ));
        assert!(state.objects.get(&island).unwrap().tapped);
        assert!(state.objects.get(&ruins).unwrap().tapped);
        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.mana[0].color, ManaType::Blue);
        assert_eq!(
            pool.mana[0].restrictions,
            vec![ManaRestriction::OnlyForSpell]
        );
    }

    #[test]
    fn filter_land_excludes_ineligible_hybrid_payment_option() {
        // CR 106.6: Of the apparent {U/B} assignments, spell-only {U} is not
        // legal for an activation while unrestricted {B} is. The engine must
        // auto-select the sole legal plan instead of publishing both options.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with_restriction(
            &mut state,
            PlayerId(0),
            ManaType::Blue,
            ManaRestriction::OnlyForSpell,
        );
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::Combination { .. },
                ..
            }
        ));
        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.count_color(ManaType::Blue), 1);
        assert_eq!(pool.count_color(ManaType::Black), 0);
        assert_eq!(
            pool.mana[0].restrictions,
            vec![ManaRestriction::OnlyForSpell]
        );
    }

    #[test]
    fn fixed_filter_land_activates_by_tapping_other_mana_source_for_sub_cost() {
        // CR 117.1d + CR 118.2 + CR 602.2b + CR 605.3a: A mana ability with a
        // mana activation cost may activate other mana abilities while paying
        // that cost. Skycloud Expanse class: "{1}, {T}: Add {W}{U}."
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
        }
        let skycloud = create_object(
            &mut state,
            CardId(502),
            PlayerId(0),
            "Skycloud Expanse".to_string(),
            Zone::Battlefield,
        );
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::White, ManaColor::Blue],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Tap,
            ],
        });
        {
            let obj = state.objects.get_mut(&skycloud).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(ability.clone());
        }

        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            skycloud,
            0,
            &ability,
        ));

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            skycloud,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(state.objects.get(&forest).unwrap().tapped);
        assert!(state.objects.get(&skycloud).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 0);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn filter_land_prompts_for_ambiguous_hybrid_mana_payment() {
        // CR 107.4e + CR 601.2h: Pool has one {U} and one {B}. Both color
        // assignments for the {U/B} hybrid are legal, so the engine pauses
        // at `PayManaAbilityMana` with both options.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        match &result {
            WaitingFor::PayManaAbilityMana { options, .. } => {
                let expected_u = vec![ManaType::Blue];
                let expected_b = vec![ManaType::Black];
                assert!(options.contains(&expected_u));
                assert!(options.contains(&expected_b));
                assert_eq!(options.len(), 2);
            }
            _ => panic!("expected PayManaAbilityMana, got {:?}", result),
        }
        // Tap MUST NOT have happened yet — cost payment is atomic: if the
        // prompt is still pending, no part of the cost has been paid.
        // (The Composite handler pays all sub-costs in order, after the
        // hybrid plan is resolved.)
        assert!(
            !state.objects.get(&ruins).unwrap().tapped,
            "source must not be tapped while mana payment is pending",
        );
    }

    #[test]
    fn filter_land_resume_with_blue_choice_produces_requested_combination() {
        // End-to-end: enter PayManaAbilityMana, pick {U}, then resume and
        // pick the {U}{U} combination. Pool debits {U} for cost, produces
        // {U}{U}.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let (options, pending) = match result {
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            } => (options, pending_mana_ability),
            other => panic!("expected PayManaAbilityMana, got {:?}", other),
        };

        let pay_result = handle_pay_mana_ability_mana(
            &mut state,
            &options,
            &pending,
            &[ManaType::Blue],
            &mut events,
        )
        .unwrap();

        // Now at ChooseManaColor::Combination, and the {U} has been debited.
        assert!(
            matches!(
                pay_result,
                WaitingFor::ChooseManaColor {
                    choice: ManaChoicePrompt::Combination { .. },
                    ..
                }
            ),
            "expected ChooseManaColor after PayManaAbilityMana",
        );
        // {U} debited, {B} still in pool (only the hybrid shard consumed one mana).
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert!(state.objects.get(&ruins).unwrap().tapped);

        let combo_pending = match pay_result {
            WaitingFor::ChooseManaColor { context, .. } => expect_mana_ability_context(context),
            other => panic!("unexpected variant: {:?}", other),
        };
        let combo_prompt = ManaChoicePrompt::Combination {
            options: vec![
                vec![ManaType::Blue, ManaType::Blue],
                vec![ManaType::Blue, ManaType::Black],
                vec![ManaType::Black, ManaType::Black],
            ],
        };
        handle_choose_mana_color(
            &mut state,
            &combo_pending,
            &combo_prompt,
            ManaChoice::Combination(vec![ManaType::Blue, ManaType::Blue]),
            &mut events,
        )
        .unwrap();

        // Produced {U}{U}; plus the {B} still floating.
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 2);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn filter_land_resume_with_black_choice_debits_black_from_pool() {
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let (options, pending) = match waiting {
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            } => (options, pending_mana_ability),
            other => panic!("expected PayManaAbilityMana, got {:?}", other),
        };

        handle_pay_mana_ability_mana(
            &mut state,
            &options,
            &pending,
            &[ManaType::Black],
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn filter_land_colored_ability_not_activatable_with_empty_pool() {
        // CR 605.3a + CR 601.2h: Payability gate — colored filter-land
        // ability must not surface as activatable when the pool has no
        // {U} or {B}.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        // Pool intentionally empty of {U}/{B}; put one {G} so pool isn't totally empty.
        seed_pool_with(&mut state, PlayerId(0), ManaType::Green, 1);

        assert!(
            !can_activate_mana_ability_now(&state, PlayerId(0), ruins, 0, &ability),
            "filter-land colored ability must be un-activatable without the mana to pay {{U/B}}",
        );
    }

    #[test]
    fn filter_land_colored_ability_activatable_with_sufficient_pool() {
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);
        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            ruins,
            0,
            &ability,
        ));
    }

    #[test]
    fn chosen_color_devotion_mana_ability_uses_activation_choice_for_count() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let nykthos = create_object(
            &mut state,
            CardId(8100),
            player,
            "Nykthos, Shrine to Nyx".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&nykthos)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let green_permanent = create_object(
            &mut state,
            CardId(8101),
            player,
            "Green Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&green_permanent).unwrap().mana_cost =
            crate::types::mana::ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 0,
            };

        let ability = make_mana_ability(ManaProduction::ChosenColor {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Devotion {
                    colors: DevotionColors::ChosenColor,
                },
            },
            contribution: ManaContribution::Base,
            fixed_alternative: None,
        });
        Arc::make_mut(&mut state.objects.get_mut(&nykthos).unwrap().abilities)
            .push(ability.clone());

        let prompt = mana_choice_prompt(&ability.effect, &state, nykthos, None, None)
            .expect("chosen-color mana should prompt for a color");
        assert!(matches!(prompt, ManaChoicePrompt::SingleColor { .. }));

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            nykthos,
            player,
            &ability,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 2);
    }

    #[test]
    fn fixed_or_chosen_color_prompt_requires_positive_production() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(8110),
            PlayerId(0),
            "Fixed or Chosen Source".to_string(),
            Zone::Battlefield,
        );

        for (count, expected_prompt) in [(0, false), (1, true)] {
            let ability = ResolvedAbility::new(
                Effect::Mana {
                    produced: ManaProduction::ChosenColor {
                        count: QuantityExpr::Fixed { value: count },
                        contribution: ManaContribution::Base,
                        fixed_alternative: Some(ManaColor::Green),
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
                vec![],
                source,
                PlayerId(0),
            );

            assert_eq!(
                mana_choice_prompt(
                    &ability.effect,
                    &state,
                    source,
                    Some(&ability),
                    Some(&ability),
                )
                .is_some(),
                expected_prompt,
                "CR 106.5: fixed-or-chosen mana count {count} must {} a color prompt",
                if expected_prompt {
                    "reach"
                } else {
                    "not reach"
                },
            );
        }
    }

    /// Issue #460 + CR 106.12a: Vorinclex's `TapsForMana` trigger must fire
    /// **once per mana-ability resolution**, not once per mana unit. Activating
    /// Nykthos for 9 green (devotion = 9) plus a single Vorinclex fire = 10
    /// green total. Pre-fix the per-`ManaAdded` trigger scan fired Vorinclex 9
    /// times → 18. Drives the real action pipeline (ActivateAbility → pay {2}
    /// from pool → ChooseManaColor → Green).
    #[test]
    fn nykthos_with_vorinclex_produces_exactly_ten_green() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        // Nykthos: {2}{T}, choose a color, add mana equal to devotion to it.
        let nykthos = create_object(
            &mut state,
            CardId(8200),
            player,
            "Nykthos, Shrine to Nyx".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&nykthos)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let nykthos_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::ChosenColor,
                        },
                    },
                    contribution: ManaContribution::Base,
                    fixed_alternative: None,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
                AbilityCost::Tap,
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&nykthos).unwrap().abilities)
            .push(nykthos_ability);

        // Vorinclex: whenever a land you control is tapped for mana, add one
        // mana of any type that land produced. Two green pips ({G}{G}).
        let vorinclex = create_object(
            &mut state,
            CardId(8201),
            player,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&vorinclex).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 5,
            };
            obj.trigger_definitions.push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    ))
                    .valid_target(TargetFilter::Controller),
            );
        }

        // Seven more single-green-pip permanents → devotion to green = 9.
        for i in 0..7 {
            let pip = create_object(
                &mut state,
                CardId(8300 + i),
                player,
                format!("Green Pip {i}"),
                Zone::Battlefield,
            );
            state.objects.get_mut(&pip).unwrap().mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        // Seed the pool with {2} so Nykthos's generic cost is paid without
        // tapping any other land (keeps the test focused on Nykthos's tap).
        seed_pool_with(&mut state, player, ManaType::Colorless, 2);

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: nykthos,
                ability_index: 0,
            },
        )
        .expect("Nykthos's {2}{T} ability should activate");
        assert!(
            matches!(result.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "expected ChooseManaColor, got {:?}",
            result.waiting_for
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            },
        )
        .expect("color choice should resolve");

        // 9 green from Nykthos (devotion) + 1 from a single Vorinclex fire = 10.
        // Pre-fix: Vorinclex fired once per `ManaAdded` (9×) → 18.
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            10,
            "Nykthos 9 + Vorinclex 1 = 10 green (NOT 18 from per-unit firing)"
        );
        assert_eq!(state.players[0].mana_pool.total(), 10);
    }

    /// Regression: a 1-mana `{T}` producer with Vorinclex out yields exactly
    /// 2 — proving the `TapsForMana` trigger fires exactly once per resolution.
    #[test]
    fn one_mana_producer_with_vorinclex_yields_exactly_two() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        let forest = create_object(
            &mut state,
            CardId(8400),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&forest).unwrap().abilities).push(
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
        );

        let vorinclex = create_object(
            &mut state,
            CardId(8401),
            player,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    ))
                    .valid_target(TargetFilter::Controller),
            );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: forest,
                ability_index: 0,
            },
        )
        .expect("Forest's {T} ability should activate");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            2,
            "1 base + 1 Vorinclex = 2 (single fire on a 1-mana producer)"
        );
    }

    /// Issue #465 — true pipeline regression for the `valid_card` controller
    /// scope on "Whenever you tap a land for mana" triggers. Drives `apply` /
    /// `apply_as_current` (`ActivateAbility` → mana resolution → trigger
    /// matching), not hand-built state.
    ///
    /// CR 603.2 + CR 106.12a: the trigger event must match a land *you* tapped,
    /// so the source filter (`valid_card`) carries `ControllerRef::You`.
    ///
    /// This test deliberately leaves `valid_target = None` so `valid_card` is
    /// the *sole* gate — isolating the issue #465 fix. (The real card also
    /// parses `valid_target = Controller`; that field independently gates
    /// `valid_player_matches`, so including it would shadow `valid_card` and
    /// the mutation-check below would not discriminate. `valid_target` does NOT
    /// route the `TriggerEventManaType` mana — `effects/mana.rs` routes that to
    /// the `TappedForMana` event's `player_id` directly — so omitting it does
    /// not change the positive-case mana total.)
    ///
    /// Mutation-check: replacing `valid_card`'s `TypedFilter::land()
    /// .controller(ControllerRef::You)` with the pre-fix unscoped
    /// `TypedFilter::land()` makes the negative assertion FAIL — the opponent's
    /// tap fires Vorinclex's triggered mana ability, adding a second green to
    /// the opponent's pool (1 base + 1 Vorinclex = 2 instead of 1). Verified.
    #[test]
    fn vorinclex_you_tap_trigger_ignores_opponent_land_tap() {
        let mut state = GameState::new_two_player(42);
        let me = PlayerId(0);
        let opp = PlayerId(1);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = me;
        state.priority_player = me;
        state.waiting_for = WaitingFor::Priority { player: me };

        // Vorinclex under PlayerId(0)'s control, with the controller-scoped
        // `valid_card` produced by the issue #465 parser fix. `valid_target` is
        // intentionally omitted (see the test's doc comment) so `valid_card` is
        // the sole gate.
        let vorinclex = create_object(
            &mut state,
            CardId(8600),
            me,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    )),
            );

        // A Forest controlled by the opponent.
        let opp_forest = create_object(
            &mut state,
            CardId(8601),
            opp,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&opp_forest).unwrap().abilities).push(
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
        );

        // A Forest controlled by Vorinclex's controller.
        let my_forest = create_object(
            &mut state,
            CardId(8602),
            me,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&my_forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&my_forest).unwrap().abilities).push(
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
        );

        // Negative case: opponent taps their Forest for mana. Vorinclex's
        // "you tap" trigger must NOT fire. Vorinclex's trigger is a triggered
        // mana ability (Effect::Mana) — when it fires, `TriggerEventManaType`
        // mana is added to the `TappedForMana` event's `player_id`, i.e. the
        // *tapping* player (CR 106.3 + CR 109.5). So the discriminating signal
        // is the OPPONENT's green pool: pre-fix (unscoped `valid_card`) the
        // opponent would receive 1 base + 1 from Vorinclex = 2; post-fix the
        // trigger does not fire, so the opponent receives 1 base only.
        // CR 605.3a: a mana ability may be activated whenever a player has
        // priority; hand priority to the opponent so they may activate.
        state.priority_player = opp;
        state.waiting_for = WaitingFor::Priority { player: opp };
        crate::game::engine::apply(
            &mut state,
            opp,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: opp_forest,
                ability_index: 0,
            },
        )
        .expect("opponent's Forest {T} ability should activate");
        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Green),
            1,
            "opponent tapping a land yields only its 1 base green — Vorinclex's \
             'you tap' trigger must not fire on an opponent's land tap"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            0,
            "Vorinclex's controller gains no mana from an opponent's land tap"
        );

        // Positive case: Vorinclex's controller taps their own Forest.
        // 1 base green + 1 from Vorinclex's trigger = 2.
        state.priority_player = me;
        state.waiting_for = WaitingFor::Priority { player: me };
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: my_forest,
                ability_index: 0,
            },
        )
        .expect("controller's Forest {T} ability should activate");
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            2,
            "1 base + 1 Vorinclex = 2 when the controller taps their own land"
        );
    }

    /// Regression: effect-produced (non-tap) mana does not fire Vorinclex.
    /// CR 106.12a — only a `{T}`-cost mana ability is "tapped for mana"; an
    /// `Effect::Mana` resolution from a spell/non-mana ability emits no
    /// `TappedForMana` event.
    #[test]
    fn effect_produced_mana_does_not_fire_vorinclex() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        let vorinclex = create_object(
            &mut state,
            CardId(8500),
            player,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Any)
                    .valid_target(TargetFilter::Controller),
            );

        // Effect-produced mana: `produce_mana` with `tapped_for_mana = false`.
        let source = create_object(
            &mut state,
            CardId(8501),
            player,
            "Mana Spell".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        mana_payment::produce_mana(
            &mut state,
            source,
            ManaType::Green,
            player,
            false,
            &mut events,
        );
        crate::game::triggers::process_triggers(&mut state, &events);

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            1,
            "effect-produced mana adds 1, Vorinclex does not fire (no TappedForMana)"
        );
    }

    #[test]
    fn pay_mana_ability_mana_rejects_unlisted_payment() {
        // Handler rejects a payment vector not present in `options`.
        let mut state = GameState::new_two_player(42);
        let (ruins, _ability) = setup_sunken_ruins(&mut state);
        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let options = vec![vec![ManaType::Blue], vec![ManaType::Black]];
        let mut events = Vec::new();
        let result = handle_pay_mana_ability_mana(
            &mut state,
            &options,
            &pending,
            &[ManaType::Red],
            &mut events,
        );
        assert!(result.is_err());
    }

    // Regression: Gemstone Mine's `{T}, Remove a mining counter` ability could
    // not activate because the replacement parser emitted "MINING" (uppercase)
    // while the cost parser emitted "mining" (lowercase), and
    // `CounterType::Generic` used the raw string as the HashMap key, so the
    // payability check found 0 counters and blocked activation.
    //
    // This fixture exercises the full depletion-land pattern — composite
    // Tap+RemoveCounter cost — so that any regression in counter-type
    // normalisation surfaces immediately. The negative test below
    // (`gemstone_mine_unpayable_without_counters`) locks in the *other*
    // direction: the payability gate must remain coupled to the canonical
    // key, so that counters going to zero correctly blocks activation
    // rather than the gate silently passing on a stale uppercase key.
    fn make_gemstone_mine(state: &mut GameState, player: PlayerId) -> ObjectId {
        let land = create_object(
            state,
            CardId(8000),
            player,
            "Gemstone Mine".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        // Seed with three mining counters via `parse_counter_type` to mirror
        // the actual effect pipeline (the ETB replacement emits "MINING" in
        // uppercase; `parse_counter_type` must normalise it to the same key
        // that the cost-payability check uses, which parses "mining" lowercase).
        // Using the uppercase spelling here exercises the normalisation fix
        // end-to-end: if the fix were reverted, the HashMap key would be
        // `Generic("MINING")` while the lookup key would be `Generic("mining")`
        // and the payability check would return false.
        let mining_key = crate::types::counter::parse_counter_type("MINING");
        obj.counters.insert(mining_key, 3);

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::RemoveCounter {
                    count: 1,
                    counter_type: CounterMatch::OfType(CounterType::Generic("mining".to_string())),
                    target: None,
                    selection: crate::types::ability::CounterCostSelection::SingleObject,
                },
            ],
        });
        Arc::make_mut(&mut obj.abilities).push(ability);
        land
    }

    #[test]
    fn gemstone_mine_activates_and_consumes_counter() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_gemstone_mine(&mut state, player);

        // Sanity: payability gate must pass while counters are present.
        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();
        assert!(
            can_activate_mana_ability_now(&state, player, land, 0, &def),
            "Gemstone Mine must be activatable while it has mining counters"
        );

        // Activate: produce green mana with the single-color override.
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            land,
            player,
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .expect("Gemstone Mine activation must not fail with counters present");

        // One green mana must land in the pool.
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Green),
            1,
            "Gemstone Mine must add one green mana on activation"
        );
        // The land must be tapped.
        assert!(
            state.objects.get(&land).unwrap().tapped,
            "Gemstone Mine must be tapped after activation"
        );
        // One mining counter must have been removed (3 → 2).
        let remaining = state
            .objects
            .get(&land)
            .unwrap()
            .counters
            .get(&CounterType::Generic("mining".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            remaining, 2,
            "Gemstone Mine must lose one mining counter per activation"
        );
    }

    #[test]
    fn any_number_counter_mana_ability_prompts_and_removes_chosen_count() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = create_object(
            &mut state,
            CardId(8002),
            player,
            "Storage Land".to_string(),
            Zone::Battlefield,
        );
        let storage = CounterType::Generic("storage".to_string());
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.counters.insert(storage.clone(), 3);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: Vec::new(),
                        grants: Vec::new(),
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::RemoveCounter {
                            count: REMOVE_COUNTER_COST_ANY_NUMBER,
                            counter_type: CounterMatch::OfType(storage.clone()),
                            target: None,
                            selection: crate::types::ability::CounterCostSelection::SingleObject,
                        },
                    ],
                }),
            );
        }

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: land,
                ability_index: 0,
            },
        )
        .expect("storage mana ability should prompt for a counter count");

        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                resource, min, max, ..
            } => {
                assert_eq!(*resource, PayableResource::Counters);
                assert_eq!(*min, 0);
                assert_eq!(*max, 3);
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::SubmitPayAmount { amount: 1 },
        )
        .expect("chosen counter count should resume mana production");

        assert_eq!(
            state.objects[&land]
                .counters
                .get(&storage)
                .copied()
                .unwrap_or(0),
            2,
            "any-number counter mana costs must remove only the chosen count"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn gemstone_mine_unpayable_without_counters() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_gemstone_mine(&mut state, player);

        // Drain all counters so the cost cannot be paid.
        let mining_key = crate::types::counter::parse_counter_type("MINING");
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .counters
            .insert(mining_key, 0);

        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();
        assert!(
            !can_activate_mana_ability_now(&state, player, land, 0, &def),
            "Gemstone Mine must not be activatable when it has no mining counters"
        );
    }

    // Issue #6507 integration follow-up: `make_gemstone_mine` above omits the
    // trailing "If there are no mining counters on this land, sacrifice it."
    // sub-ability entirely, so it never exercised the reported bug or its fix
    // through the real activation pipeline — only the parser-level AST shape
    // is covered by `oracle::tests::gemstone_mine_conditional_sacrifice_binds_to_self_ref`.
    // This fixture mirrors the actual end-to-end parsed shape (mana effect +
    // conditional `Sacrifice { target: SelfRef }` sub-ability gated on zero
    // mining counters) so activation itself proves the fix: mana is produced
    // regardless, and the land is sacrificed only on the activation that
    // removes its LAST counter.
    fn make_gemstone_mine_with_sacrifice_sub_ability(
        state: &mut GameState,
        player: PlayerId,
        initial_mining_counters: u32,
    ) -> ObjectId {
        let land = create_object(
            state,
            CardId(8003),
            player,
            "Gemstone Mine".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let mining_key = crate::types::counter::parse_counter_type("MINING");
        obj.counters.insert(mining_key, initial_mining_counters);

        let sacrifice_if_depleted = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        )
        .condition(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CountersOn {
                    scope: ObjectScope::Source,
                    counter_type: Some(CounterType::Generic("mining".to_string())),
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        });

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::RemoveCounter {
                    count: 1,
                    counter_type: CounterMatch::OfType(CounterType::Generic("mining".to_string())),
                    target: None,
                    selection: crate::types::ability::CounterCostSelection::SingleObject,
                },
            ],
        })
        .sub_ability(sacrifice_if_depleted);
        Arc::make_mut(&mut obj.abilities).push(ability);
        land
    }

    #[test]
    fn gemstone_mine_survives_activation_with_counters_remaining() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_gemstone_mine_with_sacrifice_sub_ability(&mut state, player, 2);

        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            land,
            player,
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .expect("Gemstone Mine activation must not fail with counters present");

        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Green),
            1,
            "mana must be produced regardless of the trailing conditional sacrifice"
        );
        assert!(
            state.battlefield.contains(&land),
            "Gemstone Mine must remain on the battlefield while a mining counter remains after activation"
        );
    }

    #[test]
    fn gemstone_mine_sacrifices_itself_on_last_counter_removed() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_gemstone_mine_with_sacrifice_sub_ability(&mut state, player, 1);

        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            land,
            player,
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .expect("Gemstone Mine activation must not fail on its last counter");

        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Green),
            1,
            "mana must still be produced on the activation that empties the last counter"
        );
        assert!(
            !state.battlefield.contains(&land),
            "Gemstone Mine must be sacrificed once its last mining counter is removed (issue #6507)"
        );
        assert!(
            state.players[player.0 as usize].graveyard.contains(&land),
            "the sacrificed Gemstone Mine must land in its controller's graveyard"
        );
    }

    #[test]
    fn cabal_coffers_pays_generic_taps_and_counts_swamps() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let coffers = create_object(
            &mut state,
            CardId(9001),
            player,
            "Cabal Coffers".to_string(),
            Zone::Battlefield,
        );
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Subtype("Swamp".to_string()))
                                    .controller(ControllerRef::You),
                            ),
                        },
                    },
                    color_options: vec![ManaColor::Black],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
                AbilityCost::Tap,
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&coffers).unwrap().abilities)
            .push(ability.clone());

        for idx in 0..3 {
            let swamp = create_object(
                &mut state,
                CardId(9010 + idx),
                player,
                "Swamp".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&swamp).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Swamp".to_string());
        }
        seed_pool_with(&mut state, player, ManaType::Black, 2);

        assert!(
            can_activate_mana_ability_now(&state, player, coffers, 0, &ability),
            "Cabal Coffers must be activatable with two mana available"
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, coffers, player, &ability, &mut events, None)
            .expect("Cabal Coffers activation must pay {2}, tap, and add mana");

        assert!(state.objects.get(&coffers).unwrap().tapped);
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Black),
            3
        );
    }

    /// CR 602.2a + CR 605.1a: An activated ability's controller is the
    /// player who activated it (not the owner of the source permanent).
    /// A `Controller`-scoped damage sub-effect therefore resolves against
    /// the activator — opponent-controlled painlands damage the opponent,
    /// not the original owner.
    #[test]
    fn pain_land_damage_routes_to_activator_not_original_owner() {
        let mut state = GameState::new_two_player(42);
        let brushland = create_object(
            &mut state,
            CardId(1001),
            PlayerId(1), // opponent controls it
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&brushland).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(1),
            source_id: brushland,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::SingleColor {
            options: vec![ManaType::Green, ManaType::White],
        };
        let mut events = Vec::new();

        let result = handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::SingleColor(ManaType::Green),
            &mut events,
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(
            state.players[1].life, 19,
            "activator (PlayerId(1)) should take 1 damage"
        );
        assert_eq!(
            state.players[0].life, 20,
            "non-activator (PlayerId(0)) should be unharmed"
        );
    }

    /// A 2-damage painland variant (Ancient Tomb shape) must route through
    /// the same sub-ability continuation path as the 1-damage case — the
    /// handler is parameterized over `amount`, not hardcoded.
    #[test]
    fn two_damage_painland_variant_deals_full_amount() {
        let mut state = GameState::new_two_player(42);
        let tomb = create_object(
            &mut state,
            CardId(1002),
            PlayerId(0),
            "Ancient Tomb".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&tomb).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap)
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                    damage_source: None,
                    excess: None,
                },
            )),
        );

        let ability = state.objects[&tomb].abilities[0].clone();
        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            tomb,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
        assert_eq!(
            state.players[0].life, 18,
            "Ancient Tomb should deal 2 damage to its controller"
        );
    }

    // ---------------------------------------------------------------
    // CR 605.3b + CR 605.1a: Painland-style self-damage sub-abilities
    // resolve inline with the mana ability.
    // ---------------------------------------------------------------

    fn make_painland(state: &mut GameState, player: PlayerId, color: ManaColor) -> ObjectId {
        let land = create_object(
            state,
            CardId(7000),
            player,
            "Painland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let sub = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
        );

        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        ability.sub_ability = Some(Box::new(sub));
        Arc::make_mut(&mut obj.abilities).push(ability);
        land
    }

    #[test]
    fn painland_deals_one_damage_when_tapped_for_color() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_painland(&mut state, player, ManaColor::White);
        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        let starting_life = state.players[player.0 as usize].life;
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, land, player, &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[player.0 as usize].life,
            starting_life - 1,
            "Painland must deal 1 damage to its controller"
        );
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::White),
            1,
            "Painland must still produce the colored mana"
        );
        assert!(
            state.objects.get(&land).unwrap().tapped,
            "Painland must tap"
        );
    }

    #[test]
    fn painland_kills_controller_at_one_life_via_sba_trigger() {
        // Activating the colored mana at 1 life drops the controller to 0.
        // The life-drop event must be emitted — SBAs triggered on the next
        // engine pass will eliminate the player.
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_painland(&mut state, player, ManaColor::White);
        state.players[player.0 as usize].life = 1;

        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, land, player, &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[player.0 as usize].life, 0,
            "Controller must hit 0 life after the painland damage"
        );
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::White),
            1,
            "Mana production must still occur"
        );
    }

    // ---------------------------------------------------------------------
    // CR 117.1 + CR 202.3: Cost-paid object mana value (Food Chain class)
    // ---------------------------------------------------------------------

    /// Build a Food Chain mana ability:
    /// "Exile a creature you control: Add X mana of any one color, where
    ///  X is 1 plus the exiled creature's mana value. Spend this mana only
    ///  to cast creature spells."
    fn make_food_chain_ability() -> AbilityDefinition {
        use crate::types::ability::{
            ManaSpendRestriction, ObjectScope, QuantityRef, TargetFilter as TF, TypedFilter,
        };
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue {
                                scope: ObjectScope::CostPaidObject,
                            },
                        }),
                        offset: 1,
                    },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::SpellType("Creature".to_string())],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: Some(TF::Typed(
                TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
            )),
        })
    }

    fn make_phyrexian_altar_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::creature()),
            1,
        )))
    }

    fn make_titans_nest_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(TargetFilter::Typed(
                TypedFilter::card()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }]),
            )),
        })
    }

    /// Helper: spawn `name` on the battlefield with a printed mana cost
    /// and the Creature core type.
    fn spawn_creature_with_cost(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        cost: ManaCost,
    ) -> ObjectId {
        use crate::types::card_type::{CardType, CoreType};
        let id = create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.mana_cost = cost;
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }
        id
    }

    #[test]
    fn phyrexian_altar_prompts_for_controlled_creature_then_adds_mana() {
        let mut state = GameState::new_two_player(42);
        let altar = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Phyrexian Altar".to_string(),
            Zone::Battlefield,
        );
        let ability = make_phyrexian_altar_ability();
        Arc::make_mut(&mut state.objects.get_mut(&altar).unwrap().abilities).push(ability.clone());

        let creature = spawn_creature_with_cost(
            &mut state,
            PlayerId(0),
            "Grizzly Bears",
            ManaCost::generic(2),
        );
        let opponent_creature = spawn_creature_with_cost(
            &mut state,
            PlayerId(1),
            "Runeclaw Bear",
            ManaCost::generic(2),
        );

        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), altar, 0, &ability),
            "Phyrexian Altar must be activatable when its controller has a creature to sacrifice"
        );

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            altar,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Black)),
        )
        .expect("activation should surface the sacrifice choice");

        let pending = match waiting {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::Sacrifice,
                count,
                choices: permanents,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert_eq!(permanents, vec![creature]);
                assert!(!permanents.contains(&opponent_creature));
                pending_mana_ability
            }
            other => panic!("expected PayCost Sacrifice (mana ability), got {other:?}"),
        };

        let result = handle_sacrifice_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("sacrifice choice should resolve the mana ability");

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.objects.get(&creature).unwrap().zone, Zone::Graveyard);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn titans_nest_exiles_own_graveyard_card_for_colorless_mana() {
        let mut state = GameState::new_two_player(42);
        let nest = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Titans' Nest".to_string(),
            Zone::Battlefield,
        );
        let ability = make_titans_nest_ability();
        Arc::make_mut(&mut state.objects.get_mut(&nest).unwrap().abilities).push(ability.clone());

        let own_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First graveyard card".to_string(),
            Zone::Graveyard,
        );
        let own_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second graveyard card".to_string(),
            Zone::Graveyard,
        );
        let own_stolen = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Stolen graveyard card".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&own_stolen).unwrap().controller = PlayerId(1);
        let opponent_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent graveyard card".to_string(),
            Zone::Graveyard,
        );

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            nest,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .expect("Titans' Nest should ask which graveyard card pays the cost");

        let pending = match waiting {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromManaZone { zone },
                count,
                choices: cards,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert_eq!(zone, Zone::Graveyard);
                assert!(cards.contains(&own_a));
                assert!(cards.contains(&own_b));
                assert!(cards.contains(&own_stolen));
                assert!(!cards.contains(&opponent_card));
                pending_mana_ability
            }
            other => panic!("expected PayCost ExileFromManaZone (mana ability), got {other:?}"),
        };

        let result = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[own_a, own_b],
            &pending,
            &[own_a],
            &mut events,
        )
        .expect("exile choice should resolve the mana ability");

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.objects.get(&own_a).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&own_b).unwrap().zone, Zone::Graveyard);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    #[test]
    fn exile_for_mana_ability_rejects_duplicate_selected_cards() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Two-card Exile Source".to_string(),
            Zone::Battlefield,
        );
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First graveyard card".to_string(),
            Zone::Graveyard,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second graveyard card".to_string(),
            Zone::Graveyard,
        );
        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_index: None,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };

        let result = handle_exile_for_mana_ability(
            &mut state,
            2,
            &[first, second],
            &pending,
            &[first, first],
            &mut Vec::new(),
        );

        assert!(result.is_err());
        assert_eq!(state.objects.get(&first).unwrap().zone, Zone::Graveyard);
        assert_eq!(state.objects.get(&second).unwrap().zone, Zone::Graveyard);
    }

    #[test]
    fn sacrifice_mana_cost_rejects_prohibited_selected_permanent() {
        let mut state = GameState::new_two_player(42);
        let altar = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Phyrexian Altar".to_string(),
            Zone::Battlefield,
        );
        let ability = make_phyrexian_altar_ability();
        Arc::make_mut(&mut state.objects.get_mut(&altar).unwrap().abilities).push(ability);

        let creature = spawn_creature_with_cost(
            &mut state,
            PlayerId(0),
            "Grizzly Bears",
            ManaCost::generic(2),
        );
        let lock = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Cost Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantPayCost {
                who: ProhibitionScope::AllPlayers,
                cost: CostPaymentProhibition::Sacrifice {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            }));

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: altar,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Black)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };

        let result = handle_sacrifice_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut Vec::new(),
        );

        assert!(result.is_err());
        assert_eq!(
            state.objects.get(&creature).unwrap().zone,
            Zone::Battlefield
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn sacrifice_creature_mana_cost_can_use_creature_source_itself() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Thermopod".to_string(),
            Zone::Battlefield,
        );
        let ability = make_phyrexian_altar_ability();
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Red)),
        )
        .expect("creature source should be eligible to pay its own sacrifice-a-creature cost");

        let pending = match waiting {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                count,
                choices: permanents,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(count, 1);
                assert_eq!(permanents, vec![source]);
                pending_mana_ability
            }
            other => panic!("expected PayCost Sacrifice (mana ability), got {other:?}"),
        };

        let result = handle_sacrifice_for_mana_ability(
            &mut state,
            1,
            &[source],
            &pending,
            &[source],
            &mut events,
        )
        .expect("source creature should be sacrificed and produce mana");

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.objects.get(&source).unwrap().zone, Zone::Graveyard);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
    }

    /// (a) Sacrificing a 3-mana-value creature gives 4 mana from Food Chain.
    #[test]
    fn food_chain_exiles_three_mana_value_creature_produces_four_mana() {
        let mut state = GameState::new_two_player(42);
        let chain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Food Chain".to_string(),
            Zone::Battlefield,
        );
        // Stash the food-chain ability so the dispatch can find it by index.
        Arc::make_mut(&mut state.objects.get_mut(&chain).unwrap().abilities)
            .push(make_food_chain_ability());

        // 3-MV creature: cost {2}{G}.
        let three_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        };
        let creature =
            spawn_creature_with_cost(&mut state, PlayerId(0), "Grizzly Bears", three_cost);

        // Player picks the creature to exile via the resume handler.
        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: chain,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Green)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("food chain exile handler must accept the chosen creature");

        // 1 plus mana value of {2}{G} = 4 mana.
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            4,
            "Food Chain must produce 4 green mana for a 3-MV exiled creature"
        );
        // Creature is now in exile.
        assert_eq!(
            state.objects.get(&creature).unwrap().zone,
            Zone::Exile,
            "Exiled creature must be in the exile zone after cost is paid"
        );
    }

    /// (b) Exiling a 0-mana-value creature gives 1 mana (offset = 1).
    #[test]
    fn food_chain_exiles_zero_mana_value_creature_produces_one_mana() {
        let mut state = GameState::new_two_player(42);
        let chain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Food Chain".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&chain).unwrap().abilities)
            .push(make_food_chain_ability());

        // 0-MV creature (Memnite-style): no shards, no generic.
        let zero_cost = ManaCost::Cost {
            shards: vec![],
            generic: 0,
        };
        let creature = spawn_creature_with_cost(&mut state, PlayerId(0), "Memnite", zero_cost);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: chain,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Red)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("food chain exile handler must accept the 0-MV creature");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            1,
            "Food Chain must produce 1 red mana for a 0-MV exiled creature"
        );
    }

    /// (c) Burnt-Offering / Metamorphosis class — an `AbilityResolution`
    /// stamped with a captured mana value resolves
    /// `ObjectManaValue { CostPaidObject }` to that value at production time.
    #[test]
    fn cost_paid_object_resolves_via_resolved_ability_field() {
        use crate::game::quantity::resolve_quantity_with_targets;
        use crate::types::ability::{CostPaidObjectSnapshot, ObjectScope, QuantityRef};

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    },
                    color_options: vec![ManaColor::Black, ManaColor::Red],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut paid = crate::game::game_object::GameObject::new(
            ObjectId(99),
            CardId(99),
            PlayerId(0),
            "Paid Creature".to_string(),
            Zone::Battlefield,
        );
        paid.mana_cost = crate::types::mana::ManaCost::generic(5);
        ability.set_cost_paid_object_recursive(CostPaidObjectSnapshot {
            object_id: paid.id,
            lki: paid.snapshot_for_mana_spent(),
        });

        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            resolved, 5,
            "CostPaidObject must resolve to the captured mana value"
        );
    }

    /// Resolver returns 0 when no cost-paid object snapshot is in scope —
    /// regression guard that avoids spurious mana production for unrelated
    /// abilities.
    #[test]
    fn cost_paid_object_returns_zero_without_snapshot() {
        use crate::game::quantity::resolve_quantity_with_targets;
        use crate::types::ability::{ObjectScope, QuantityRef};

        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
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
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        // No `set_cost_paid_object_recursive` — field stays None.

        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            resolved, 0,
            "CostPaidObject must return 0 when no snapshot was captured"
        );
    }

    /// Food Chain mana carries `ManaSpendRestriction::SpellType("Creature")`
    /// so the produced mana cannot pay non-creature spell costs.
    #[test]
    fn food_chain_mana_is_creature_spell_only() {
        let mut state = GameState::new_two_player(42);
        let chain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Food Chain".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&chain).unwrap().abilities)
            .push(make_food_chain_ability());

        let three_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        };
        let creature =
            spawn_creature_with_cost(&mut state, PlayerId(0), "Grizzly Bears", three_cost);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: chain,
            ability_index: Some(0),
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Green)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("food chain exile handler must accept the chosen creature");

        // Every produced unit must carry the SpellType("Creature") restriction.
        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.total(), 4);
        for unit in &pool.mana {
            assert_eq!(
                unit.restrictions,
                vec![crate::types::mana::ManaRestriction::OnlyForSpellType(
                    "Creature".to_string()
                )],
                "Food Chain mana must carry the Creature spell-type restriction"
            );
        }
    }

    /// CR 602.5: the mana-ability executor must reject submissions that violate
    /// an active `CantActivateDuring` static, not only the legal-action filter.
    /// Discriminating end-to-end test against the City of Solitude class: a
    /// hostile/buggy client submitting `activate_mana_ability` directly must
    /// receive `EngineError::ActionNotAllowed`.
    #[test]
    fn city_of_solitude_rejects_mana_ability_at_executor() {
        use crate::types::statics::{ActivationExemption, CastingProhibitionCondition};

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        state.active_player = p0;
        state.phase = Phase::PreCombatMain;

        // P0 controls a City of Solitude analogue (AllPlayers / NotDuringAffectedPlayersTurn
        // / exemption: None — per the 2009-10-01 ruling).
        let prohibitor = create_object(
            &mut state,
            CardId(1),
            p0,
            "City of Solitude".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prohibitor)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantActivateDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::NotDuringAffectedPlayersTurn,
                exemption: ActivationExemption::None,
            }));

        // P1 controls a Forest-like permanent with a tap-for-green mana ability.
        let forest = create_object(
            &mut state,
            CardId(2),
            p1,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let mana_ability = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut state.objects.get_mut(&forest).unwrap().abilities)
            .push(mana_ability.clone());

        // On P0's turn, P1 attempts to activate the mana ability directly through
        // the executor. The CR 602.5 gate at the top of `activate_mana_ability`
        // must reject before any cost is paid or mana is produced.
        let mut events = Vec::new();
        let err = activate_mana_ability(
            &mut state,
            forest,
            p1,
            0,
            &mana_ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .expect_err("City of Solitude must reject P1's mana ability at the executor on P0's turn");
        assert!(
            matches!(err, EngineError::ActionNotAllowed(_)),
            "expected ActionNotAllowed, got {err:?}"
        );
        // No mana was produced and the ability source was not tapped.
        assert_eq!(state.players[1].mana_pool.total(), 0);
        assert!(!state.objects.get(&forest).unwrap().tapped);
        assert!(events.is_empty());
    }

    /// Perf-gate correctness (`ManaActivationGates`, Fix A): when a
    /// CantActivateDuring (City of Solitude class) static is present the hoisted
    /// gate flag is set, so the per-source readiness scan must still run and
    /// report the mana ability UNAVAILABLE. Exercises the `gate=true` arm of
    /// `mana_ability_ready_without_simulation_gated` that the board-global mana
    /// display sweep depends on — without this the fast tests only cover the
    /// `gate=false` (no-prohibition) arm.
    #[test]
    fn can_activate_mana_ability_now_respects_cant_activate_during_via_gate() {
        use crate::types::statics::{ActivationExemption, CastingProhibitionCondition};

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        state.active_player = p0; // NOT p1's turn
        state.phase = Phase::PreCombatMain;

        // P0 controls a City of Solitude analogue (AllPlayers /
        // NotDuringAffectedPlayersTurn / exemption: None).
        let prohibitor = create_object(
            &mut state,
            CardId(1),
            p0,
            "City of Solitude".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prohibitor)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantActivateDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::NotDuringAffectedPlayersTurn,
                exemption: ActivationExemption::None,
            }));

        let forest = create_object(
            &mut state,
            CardId(2),
            p1,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let mana_ability = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut state.objects.get_mut(&forest).unwrap().abilities)
            .push(mana_ability.clone());

        // gate=true arm: prohibition exists → scan runs → unavailable on P0's turn.
        assert!(
            !can_activate_mana_ability_now(&state, p1, forest, 0, &mana_ability),
            "City of Solitude must make P1's mana ability unavailable on P0's turn (gate=true)"
        );

        // Control: on the affected player's own turn the prohibition lifts.
        state.active_player = p1;
        assert!(
            can_activate_mana_ability_now(&state, p1, forest, 0, &mana_ability),
            "on the affected player's own turn the mana ability is available again"
        );
    }

    // ---------------------------------------------------------------
    // Standalone RemoveCounter mana ability (Pentad Prism class)
    // ---------------------------------------------------------------

    /// Pentad Prism: `Remove a charge counter from ~: Add one mana of any color.`
    /// The cost is a bare `RemoveCounter` (NOT inside `Composite`).
    fn make_pentad_prism(state: &mut GameState, player: PlayerId) -> ObjectId {
        let prism = create_object(
            state,
            CardId(8100),
            player,
            "Pentad Prism".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&prism).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        let charge_key = crate::types::counter::parse_counter_type("charge");
        obj.counters.insert(charge_key, 2);

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::RemoveCounter {
            count: 1,
            counter_type: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
            target: None,
            selection: crate::types::ability::CounterCostSelection::SingleObject,
        });
        Arc::make_mut(&mut obj.abilities).push(ability);
        prism
    }

    #[test]
    fn standalone_remove_counter_mana_ability_activates() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let prism = make_pentad_prism(&mut state, player);

        let def = state
            .objects
            .get(&prism)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        // Readiness must pass — the prism has charge counters.
        assert!(
            can_activate_mana_ability_now(&state, player, prism, 0, &def),
            "Pentad Prism must be activatable with charge counters"
        );

        // Activate: produce blue mana.
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            prism,
            player,
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Blue)),
        )
        .expect("Standalone RemoveCounter mana ability must not fail");

        // One blue mana in pool.
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Blue),
            1,
        );
        // One charge counter removed (2 → 1).
        let remaining = state
            .objects
            .get(&prism)
            .unwrap()
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(remaining, 1);
        // Source is NOT tapped (no tap cost).
        assert!(
            !state.objects.get(&prism).unwrap().tapped,
            "Pentad Prism must not be tapped — cost is only RemoveCounter"
        );
    }

    #[test]
    fn standalone_remove_counter_mana_ability_unpayable_without_counters() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let prism = make_pentad_prism(&mut state, player);

        // Remove all charge counters.
        state.objects.get_mut(&prism).unwrap().counters.clear();

        let def = state
            .objects
            .get(&prism)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        // Readiness must fail — no counters to remove.
        assert!(
            !can_activate_mana_ability_now(&state, player, prism, 0, &def),
            "Pentad Prism must not be activatable without charge counters"
        );
    }

    /// Issue #6494 (shared seam): `casting::find_non_self_discard` is the SOLE
    /// detector for FromHand discard cost legs and no longer filters by selection
    /// mode — it returns the `CardSelectionMode` for BOTH `Chosen` and `Random`
    /// legs (and recurses into `Composite`, e.g. Lion's Eye Diamond's shape). The
    /// mana path's only divergence from the casting/activation path is the explicit
    /// `Chosen` gate in `discard_cost_choice`, which routes accepted legs through
    /// the shared `resolve_non_self_discard_requirement` authority so the zero-count
    /// auto-pay + payability rules live in one place.
    ///
    /// Revert-sensitive: fails if `find_non_self_discard` re-adds a selection filter
    /// (the `Random` assertions go `None`), or if `discard_cost_choice` drops its
    /// `Chosen` gate (the `Random` leg would then surface an interactive selection).
    #[test]
    fn find_non_self_discard_is_sole_detector_mana_path_gates_on_chosen() {
        use crate::types::ability::{CardSelectionMode, DiscardSelfScope};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        // One card in hand so a `Chosen` discard resolves to an interactive selection.
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );

        let discard_leg = |selection| AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            selection,
            self_scope: DiscardSelfScope::FromHand,
        };
        let chosen_leg = discard_leg(CardSelectionMode::Chosen);
        let random_leg = discard_leg(CardSelectionMode::Random);
        // LED-shaped composite: the Chosen discard leg lives beside a self-sacrifice.
        let chosen_in_composite = AbilityCost::Composite {
            costs: vec![
                chosen_leg.clone(),
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        };

        // Sole detector: reports the selection mode for BOTH variants (no selection
        // filter), including inside a Composite.
        let detect = crate::game::casting::find_non_self_discard;
        assert!(matches!(
            detect(&chosen_leg),
            Some((_, _, CardSelectionMode::Chosen))
        ));
        assert!(matches!(
            detect(&chosen_in_composite),
            Some((_, _, CardSelectionMode::Chosen))
        ));
        assert!(matches!(
            detect(&random_leg),
            Some((_, _, CardSelectionMode::Random))
        ));

        // Mana selection gate: only a Chosen leg surfaces an interactive discard,
        // and it does so through the shared resolver (Some((1, [card]))).
        match discard_cost_choice(&state, PlayerId(0), source, &Some(chosen_in_composite)) {
            Some((count, cards)) => {
                assert_eq!(count, 1);
                assert_eq!(cards, vec![card]);
            }
            None => panic!("Chosen FromHand discard with a card in hand must surface a selection"),
        }
        // A non-Chosen (Random) FromHand discard is not a mid-activation card
        // selection: the gate returns None even though the sole detector matched it.
        assert!(discard_cost_choice(&state, PlayerId(0), source, &Some(random_leg)).is_none());
    }

    fn ledger_event(source_id: u64) -> GameEvent {
        GameEvent::EffectResolved {
            kind: crate::types::ability::EffectKind::NoOp,
            source_id: ObjectId(source_id),
            subject: None,
        }
    }

    #[test]
    fn nested_mana_cursor_starts_with_empty_frame_local_ledger() {
        let mut parent_cursor = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            None,
        );
        parent_cursor.deferred_cost_events.push(ledger_event(1));
        let parent = ManaAbilityCostParent {
            pending: Box::new(pending_for(ObjectId(10))),
            cursor: Box::new(parent_cursor),
            lifecycle: ManaAbilityCostParentLifecycle::Synchronous,
            current_action_event_start: 0,
        };

        let child = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            Some(&parent),
        );

        assert!(child.deferred_cost_events.is_empty());
        assert_eq!(
            child
                .parent
                .as_deref()
                .expect("child must retain its parent snapshot")
                .cursor
                .deferred_cost_events,
            [ledger_event(1)]
        );
    }

    #[test]
    fn suspended_child_ledger_extends_parent_without_loss_or_overwrite() {
        let mut parent = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            None,
        );
        parent.deferred_cost_events = vec![ledger_event(1), ledger_event(2)];
        let mut child = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            None,
        );
        child.deferred_cost_events = vec![ledger_event(3)];

        append_suspended_child_cost_events(&mut parent, &mut child, &[ledger_event(4)]);

        assert_eq!(
            parent.deferred_cost_events,
            [
                ledger_event(1),
                ledger_event(2),
                ledger_event(3),
                ledger_event(4)
            ]
        );
        assert!(child.deferred_cost_events.is_empty());
    }

    #[test]
    fn repeated_pause_transfer_preserves_local_suffix_boundary() {
        let mut active_cursor = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            None,
        );
        active_cursor.deferred_cost_events = vec![ledger_event(5), ledger_event(8)];
        active_cursor.current_action_deferred_start = 1;
        let mut state = GameState::new_two_player(42);
        state.pending_cost_move_resume = Some(PendingCostMoveResume::ManaAbilityPayment {
            pending: Box::new(pending_for(ObjectId(10))),
            cursor: active_cursor,
        });

        defer_cost_events_into_active_mana_root(
            &mut state,
            vec![ledger_event(1), ledger_event(2)],
            &[ledger_event(7), ledger_event(8)],
        );

        let Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) =
            state.pending_cost_move_resume.as_ref()
        else {
            panic!("expected active mana root");
        };
        assert_eq!(
            cursor.deferred_cost_events,
            [
                ledger_event(5),
                ledger_event(1),
                ledger_event(2),
                ledger_event(7),
                ledger_event(8)
            ]
        );
        assert_eq!(cursor.current_action_deferred_start, 4);
    }

    fn synchronous_parent_at(start: usize, ledger: Vec<GameEvent>) -> ManaAbilityCostParent {
        let mut cursor = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            None,
        );
        cursor.deferred_cost_events = ledger;
        ManaAbilityCostParent {
            pending: Box::new(pending_for(ObjectId(10))),
            cursor: Box::new(cursor),
            lifecycle: ManaAbilityCostParentLifecycle::Synchronous,
            current_action_event_start: start,
        }
    }

    /// CR 603.2 + CR 603.3b: The prepared child-facing snapshot owns the live
    /// parent frame's current unscanned suffix, while the live parent cursor is
    /// never mutated. A synchronous child that drops the snapshot therefore
    /// leaves exactly one representation of those events — the reducer vector.
    #[test]
    fn prepared_parent_snapshot_carries_current_suffix_without_mutating_live_parent() {
        let parent = synchronous_parent_at(0, Vec::new());
        let events = vec![ledger_event(1), ledger_event(2)];

        let prepared = parent_snapshot_with_current_cost_events(&parent, &events);

        assert_eq!(
            prepared.cursor.deferred_cost_events,
            [ledger_event(1), ledger_event(2)],
            "prepared snapshot must carry the parent's current unscanned suffix"
        );
        assert!(
            parent.cursor.deferred_cost_events.is_empty(),
            "the live parent cursor must not be mutated by preparation"
        );

        let child = mana_ability_cost_cursor(
            &None,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::AutoResolved,
            Some(&prepared),
        );
        assert!(
            child.deferred_cost_events.is_empty(),
            "a nested child's top-level ledger starts empty"
        );
        assert_eq!(
            child
                .parent
                .as_deref()
                .expect("child retains its prepared parent")
                .cursor
                .deferred_cost_events,
            [ledger_event(1), ledger_event(2)]
        );
    }

    /// CR 603.2 + CR 603.3b: Preparation runs at every child entry from the
    /// unchanged ephemeral parent, so a later child sees the parent's own prefix
    /// plus every earlier synchronously completed sibling exactly once. Extending
    /// a previously prepared snapshot instead would double-append the prefix.
    #[test]
    fn later_child_snapshot_includes_earlier_synchronous_sibling_events_once() {
        let parent = synchronous_parent_at(0, Vec::new());
        let mut events = vec![ledger_event(1)];

        let first = parent_snapshot_with_current_cost_events(&parent, &events);
        assert_eq!(first.cursor.deferred_cost_events, [ledger_event(1)]);

        // The first child completed synchronously and emitted its own event.
        events.push(ledger_event(2));
        let second = parent_snapshot_with_current_cost_events(&parent, &events);

        assert_eq!(
            second.cursor.deferred_cost_events,
            [ledger_event(1), ledger_event(2)],
            "a later child prepares from the unchanged ephemeral parent, not from an earlier snapshot"
        );
    }

    /// CR 603.2 + CR 603.3b: The parent frame's own `cost_event_start` is the
    /// marker, so a parent that already scanned an earlier prefix contributes
    /// only its unscanned suffix to the child snapshot.
    #[test]
    fn prepared_parent_snapshot_starts_at_the_parent_frame_marker() {
        let parent = synchronous_parent_at(1, Vec::new());
        let events = vec![ledger_event(1), ledger_event(2), ledger_event(3)];

        let prepared = parent_snapshot_with_current_cost_events(&parent, &events);

        assert_eq!(
            prepared.cursor.deferred_cost_events,
            [ledger_event(2), ledger_event(3)]
        );
    }

    /// CR 603.2 + CR 603.3b: A pause makes the prepared prefix durable, so the
    /// ephemeral marker is never consulted again and is deliberately not
    /// serialized. Round-tripping a suspended parent keeps the prefix while the
    /// marker resets to its default.
    #[test]
    fn suspended_parent_prefix_survives_serde_while_marker_is_skipped() {
        let mut parent = synchronous_parent_at(0, Vec::new());
        parent = parent_snapshot_with_current_cost_events(&parent, &[ledger_event(1)]);
        parent.lifecycle = ManaAbilityCostParentLifecycle::Suspended;
        parent.current_action_event_start = 7;

        let json = serde_json::to_string(&parent).expect("serialize suspended parent");
        assert!(
            !json.contains("current_action_event_start"),
            "the ephemeral marker must not be serialized: {json}"
        );
        let restored: ManaAbilityCostParent =
            serde_json::from_str(&json).expect("deserialize suspended parent");

        assert_eq!(
            restored.cursor.deferred_cost_events,
            [ledger_event(1)],
            "the durable prefix must survive suspension"
        );
        assert_eq!(restored.current_action_event_start, 0);
        assert!(matches!(
            restored.lifecycle,
            ManaAbilityCostParentLifecycle::Suspended
        ));
    }
}

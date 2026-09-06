use crate::game::effects::add_target_replacement::ReplacementDurationExpiry;
use crate::game::effects::choose_damage_source;
use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    CombatDamageScope, DamageTargetFilter, DamageTargetPlayerScope, Effect, EffectError,
    EffectKind, FilterProp, PreventionAmount, PreventionScope, ReplacementDefinition,
    ResolvedAbility, SubAbilityLink, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};
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
pub(crate) fn resolve_source_filter(
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
        TargetFilter::ChosenDamageSource { .. } => state
            .last_chosen_damage_source
            .as_ref()
            .map(|choice| {
                let identity = TargetFilter::SpecificObject {
                    id: choice.source_id,
                };
                match &choice.source_filter {
                    TargetFilter::ChosenDamageSource { .. } | TargetFilter::Any => identity,
                    other => {
                        let recheck =
                            resolve_source_filter(other, state, source_id, ability_targets);
                        if matches!(recheck, TargetFilter::Any) {
                            identity
                        } else {
                            TargetFilter::And {
                                filters: vec![identity, recheck],
                            }
                        }
                    }
                }
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
            // CR 608.2d: Resolve IsChosenColor -> concrete HasColor using
            // the source's CURRENT chosen color. The shield is resolved into a
            // concrete filter once, when it is CREATED (this function), so it
            // must read whichever answer is current at that moment — the same
            // "current answer" `game/filter.rs`'s two `IsChosenColor` arms read.
            let chosen_color = state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.current_chosen_color());
            let mut resolved = tf.clone();
            resolved
                .properties
                .retain(|p| !matches!(p, FilterProp::IsChosenColor));
            if let Some(color) = chosen_color {
                resolved.properties.push(FilterProp::HasColor { color });
            }
            TargetFilter::Typed(resolved)
        }
        // CR 608.2c + CR 615: a bare ParentTarget damage-source filter (the "by"
        // half of a bidirectional Maze-of-Ith-class shield) captures the same
        // object the parent's own instruction selected, exactly like
        // ParentTargetSlot but without an explicit index. Issue #1094.
        TargetFilter::ParentTarget => crate::game::effects::first_object_target(ability_targets)
            .map(|id| TargetFilter::SpecificObject { id })
            .unwrap_or(TargetFilter::None),
        _ => filter.clone(),
    }
}

/// CR 113.7a + CR 611.2a: install a player-scoped prevention shield ("prevent all
/// damage that would be dealt to target player this turn").
///
/// Delegates to the one floating-install authority
/// (`effects::install_floating_damage_replacement`). This function used to fork on
/// storage -- object-hosted when the source was an object on the battlefield or in
/// the command zone, registry-hosted otherwise -- which made the shield's lifetime
/// an accident of the SOURCE'S zone and of the next CR 613.1 layer pass rather
/// than of the stated duration. `Zone::Battlefield | Zone::Command` is exactly the
/// pair this caller used for that fork, so it is passed through as THIS caller's
/// `anchor_zones`: the population it newly moves is the population it anchors.
///
/// Incidental fix: the old registry arm pushed WITHOUT latching
/// `source_controller`, so a controller-relative gate on a player-scoped shield
/// resolved against `state.active_player`. The authority latches it
/// unconditionally (CR 113.8).
fn push_player_scoped_shield(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    shield: ReplacementDefinition,
) {
    crate::game::effects::install_floating_damage_replacement(
        state,
        shield,
        controller,
        source_id,
        &[Zone::Battlefield, Zone::Command],
    );
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
        // CR 615 + CR 614.1a: "you and [type] permanents you control" (Comeuppance,
        // Channel Harm) lowers to the dedicated player-OR-controlled-permanents
        // damage filter so BOTH legs are matched. Routing this through the
        // object-only `valid_card` slot would silently drop the player ("you")
        // leg, so it must yield `Some` here (and `typed_recipient_valid_card_filter`
        // returns `None` for it) — the shield's controller is the recipient player.
        //
        // CR 109.1: the "other" article is carried straight through (The
        // Wanderer's "you and OTHER permanents you control" must not prevent
        // damage dealt to The Wanderer itself).
        TargetFilter::ControllerAndControlledPermanents {
            permanent_type,
            source_scope,
        } => Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
            player: DamageTargetPlayerScope::Controller,
            permanent_type: *permanent_type,
            source_scope: *source_scope,
        }),
        // CR 608.2c + CR 611.2c + CR 615.11 (issue #6682): a tracked-set
        // recipient ("those permanents"/"those creatures" — Mutational
        // Advantage's clause-derived population, Energy Arc's target-derived
        // untapped creatures) is an OBJECT population, not a player. The
        // generic `is_context_ref()` classification below (used broadly for
        // "does this need a declared target slot") also happens to cover
        // `TrackedSet`/`TrackedSetFiltered`, which would otherwise
        // misroute it through `resolve_player_for_context_ref` here — object
        // matching is `typed_recipient_valid_card_filter`'s job, so this
        // arm must be checked BEFORE the generic `is_context_ref()` catch-all.
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => None,
        // CR 615 + CR 201.5: the printed-name self-reference ("prevent all
        // damage that would be dealt to HIM this turn" — Gideon Jura, Gideon of
        // the Trials) names the source OBJECT, not a player. `SelfRef` is in
        // `is_context_ref()`, so without this arm the catch-all below would
        // lower it to a PLAYER shield on the source's controller — preventing
        // all damage to the player instead of to the Gideon. Object matching is
        // `typed_recipient_valid_card_filter`'s job, so this arm must precede
        // the generic `is_context_ref()` catch-all (same ordering contract as
        // the `TrackedSet` carve-out above).
        TargetFilter::SelfRef => None,
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
        // CR 615 + CR 614.1a: the compound "you and permanents you control"
        // recipient is a player+permanent shape handled entirely by
        // `untargeted_damage_filter`; it must NEVER route to the object-only
        // `valid_card` slot (that would drop the "you" leg — the HIGH-severity
        // leak this arm forecloses even if the caller's branch order changes).
        TargetFilter::ControllerAndControlledPermanents { .. } => None,
        // CR 608.2c + CR 611.2c + CR 615.11 (issue #6682): a tracked-set
        // recipient IS an object population — checked before the generic
        // `is_context_ref()` exclusion below (which would otherwise reject
        // it, mirroring `untargeted_damage_filter`'s matching carve-out).
        filter @ (TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }) => {
            Some(filter.clone())
        }
        // CR 615 + CR 201.5: the printed-name self-reference IS an object
        // recipient — the shield rides on the source permanent (the untargeted
        // branch of `resolve`) and `valid_card: SelfRef` scopes it to damage
        // dealt to that host. Checked before the generic `is_context_ref()`
        // exclusion below, which would otherwise reject it; mirrors the
        // `TrackedSet` carve-out and pairs with `untargeted_damage_filter`'s
        // matching `SelfRef => None` arm.
        filter @ TargetFilter::SelfRef => Some(filter.clone()),
        filter if filter.is_context_ref() => None,
        filter => Some(filter.clone()),
    }
}

/// CR 615 + CR 611.2a: Prevent damage — creates a prevention shield.
///
/// The shield is a `ReplacementDefinition` with `ShieldKind::Prevention` (or
/// `PreventionOneShot`), and it lands in ONE of two stores, never both:
///
/// * RECIPIENT-scoped ("prevent the next N damage that would be dealt to target
///   creature") -> the TARGET object's live `replacement_definitions`, installed
///   through `GameObject::install_resolution_replacement` so the CR 613.1 layer
///   reset carries it (CR 611.2c: a prevention effect is not a characteristic).
///   It correctly dies when its host changes zones (CR 400.7).
/// * SOURCE-scoped ("prevent all combat damage that would be dealt by that
///   creature", Circle of Protection, Mercenaries) and PLAYER-scoped ->
///   `state.pending_damage_replacements`, through the one authority
///   `effects::install_floating_damage_replacement`. CR 113.7a: once activated,
///   the ability -- and the continuous effect it created -- exists independently
///   of its source, so the shield must not ride on the source permanent.
///
/// `damage_done_applier` in `replacement.rs` consumes shields from either store
/// when matching `ProposedEvent::Damage`. Lifetime is CR 615.3 -- "until they're
/// used up or their duration has expired" -- enforced by the three `turns.rs`
/// prunes (cleanup, end-of-combat teardown, untap step), which key on `expiry`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, amount_dynamic, target, scope, effect_source_filter, prevention_duration) =
        match &ability.effect {
            Effect::PreventDamage {
                amount,
                amount_dynamic,
                target,
                scope,
                damage_source_filter,
                prevention_duration,
            } => (
                *amount,
                amount_dynamic.clone(),
                target.clone(),
                *scope,
                damage_source_filter.clone(),
                prevention_duration.clone(),
            ),
            _ => {
                return Err(EffectError::InvalidParam(
                    "expected PreventDamage effect".to_string(),
                ))
            }
        };

    // CR 608.2c + CR 611.2c + CR 615.11 (issue #6682): resolve any
    // `TrackedSet` sentinel in the recipient/source filters to a CONCRETE
    // tracked-set id now, at shield-creation time — before it is folded into
    // a `ReplacementDefinition` that may persist and be rechecked at many
    // later damage events this turn. Left unresolved, the raw
    // `TrackedSetId(0)` sentinel would be re-resolved against
    // `state.chain_tracked_set_id` at EACH future check
    // (`filter::matches_target_filter`), which drifts to whatever unrelated
    // chain most recently published a tracked set by then — a stale,
    // silently-wrong rebinding (Energy Arc's "those creatures" shield must
    // stay bound to the untapped creatures for the rest of the turn, not
    // whatever some later spell's chain happens to publish). Mirrors
    // `register_transient_effect`'s identical one-shot resolution
    // (`game/effects/effect.rs`) for the analogous durable continuous-effect
    // case. A no-op for every non-`TrackedSet` filter.
    let target = crate::game::targeting::resolve_tracked_set_sentinel(state, target);
    let effect_source_filter = effect_source_filter
        .map(|filter| crate::game::targeting::resolve_tracked_set_sentinel(state, filter));

    // CR 609.7 + CR 609.7a: A source-scoped prevent ("prevent all damage target
    // instant or sorcery spell would deal this turn") carries its chosen source
    // object in `ability.targets[0]` via a `ParentTargetSlot` sentinel in the
    // source filter. Those targets are the damage SOURCE, not a recipient — so
    // the shield must NOT be hosted on them as a recipient object. It routes to
    // the untargeted branch (pending registry) scoped via `damage_source_filter`.
    //
    // CR 615 + CR 608.2c (issue #1094): the "by"-only half of a bidirectional
    // Maze-of-Ith-class shield carries a bare `ParentTarget` source filter (the
    // chosen creature IS the damage source, not a recipient). Same routing: the
    // shield is source-scoped, so it must NOT be hosted on the creature as a
    // recipient object (which would wrongly re-impose "recipient == creature").
    //
    // CR 608.2c + CR 615 (issue #6682): Energy Arc's "by"-only half carries a
    // `TrackedSet` source filter instead (a SET of chosen creatures, not one).
    // `ability.targets` here is NOT this effect's own recipient selection — it
    // is the enclosing Untap clause's targets, inherited onto this
    // SequentialSibling by the chain walker's generic `should_propagate_
    // parent_targets` (the inheritance exists for OTHER riders that genuinely
    // want the parent's chosen object; a source-scoped shield has no use for
    // it). Without this arm, `host_on_targets` saw a non-`Any`-context-ref
    // `target` (`Any` itself isn't in `is_context_ref()`'s list) plus those
    // inherited targets and wrongly hosted the shield ON the untapped
    // creature with a forced `valid_card: SelfRef` — recipient-scoping a
    // shield that was supposed to be source-scoped only.
    let source_scoped_prevent = matches!(
        &effect_source_filter,
        Some(TargetFilter::And { filters })
            if filters
                .iter()
                .any(|f| matches!(f, TargetFilter::ParentTargetSlot { .. }))
    ) || matches!(
        &effect_source_filter,
        Some(
            TargetFilter::ParentTarget
                | TargetFilter::TrackedSet { .. }
                | TargetFilter::TrackedSetFiltered { .. }
        )
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

    // CR 615.1a + CR 615.3 + CR 614.1a: A one-shot "the next time [target
    // creature] would deal damage this turn, prevent that damage" shield (Awe
    // Strike) is recognized by its EXACT source-filter shape — the shared
    // `is_oneshot_target_source_prevent_shape` predicate (the single authority,
    // also consumed by the parser-side bare-rider gate in assembly.rs). Only
    // this shape is one-shot: Dromoka's Command's source-scoped shield
    // (`Typed(instant|sorcery)` leaf) is a duration-bound continuous
    // `Prevention { All }` that must keep re-firing, so it does NOT match here.
    //
    // NOTE: `target: Any` on the parsed effect is deliberately not consulted on
    // the `source_scoped_prevent` path below — the target slot is carried by
    // the `damage_source_filter`'s `And`, and `Any` simply means "no recipient
    // scope" (CR 115.1: the target slot is hosted by the source-filter `And`).
    let oneshot_source_shape = effect_source_filter
        .as_ref()
        .is_some_and(crate::types::ability::is_oneshot_target_source_prevent_shape);
    if oneshot_source_shape {
        // CR 615.3: the single opportunity is bounded by the "the next time"
        // qualifier — consumed on apply; CR 514.2: expires at cleanup.
        shield.consume_on_apply = true;
        // Builder, not a direct field write, so the one-shot path picks up the
        // builder's CR 514.2 `EndOfTurn` stamp and there is one construction
        // authority for the kind and its window.
        shield = shield.prevention_oneshot_shield();
    }

    // CR 611.2a + CR 608.2: "a continuous effect generated by the resolution of a
    // spell or ability lasts as long as stated by the SPELL OR ABILITY creating
    // it." That sentence names two carriers, and this engine stores them
    // separately: the effect grammar's own window (`prevention_duration` — "this
    // combat" -> EndOfCombat) and the resolving ability's window
    // (`ability.duration` — "Until your next turn" -> UntilPlayerNextTurn). Read
    // the effect-level carrier first, then fall back to the ability-level one.
    //
    // CR 511.2 + CR 615: "this combat" -> `RestrictionExpiry::EndOfCombat`, pruned
    // at the EndCombat phase (turns.rs) so a Suppressor Skyguard shield from
    // combat 1 does not bleed into a second combat the same turn. Skyguard's
    // window rides on `ability.duration`, so it is the `.or_else` arm — not the
    // first one — that makes that statement true. (Its shield is object-hosted
    // and is destroyed by the next layer flush before the corrected window can be
    // observed; that is a separate, pre-existing defect.)
    //
    // Engine default, LAST: see `ReplacementDefinition::with_resolution_shield_expiry`
    // — an end-of-turn fallback that compensates for the parser dropping a printed
    // "this turn", NOT a CR rule (CR 611.2a's no-duration case is "until the end
    // of the game"). Without it, `turns::execute_cleanup` — which reads `expiry`
    // alone — would leave every duration-less resolution shield immortal.
    // CR 611.2a: refuse a stated lifetime this seam cannot enforce BEFORE the
    // shield is built, from the one authority the parse-time honesty net also
    // reads (`parser::oracle::demote_unenforceable_replacement_lifetimes`), so
    // the two can never disagree about which shape is supported.
    if prevention_shield_is_refused(prevention_duration.as_ref(), ability.duration.as_ref()) {
        return Err(EffectError::InvalidParam(format!(
            "PreventDamage: no enforceable lifetime for stated duration {:?} (CR 611.2a); \
             the parser must lower this line to Effect::Unimplemented instead",
            prevention_duration.as_ref().or(ability.duration.as_ref())
        )));
    }
    let expiry = match crate::game::effects::add_target_replacement::expiry_from_duration(
        prevention_duration.as_ref(),
    ) {
        ReplacementDurationExpiry::Unstated => {
            crate::game::effects::add_target_replacement::expiry_from_duration(
                ability.duration.as_ref(),
            )
        }
        expiry => expiry,
    };
    match expiry {
        ReplacementDurationExpiry::Explicit(expiry) => shield = shield.expiry(expiry),
        // CR 611.2a: the controller-scoped class, named here rather than
        // re-derived inside the shared classification.
        ReplacementDurationExpiry::ExplicitControllerNextTurn => {
            shield = shield.expiry(
                crate::types::ability::RestrictionExpiry::UntilPlayerNextTurn {
                    player: ability.controller,
                },
            );
        }
        ReplacementDurationExpiry::Unstated => {
            shield = shield.with_resolution_shield_expiry();
        }
        // Unreachable: `prevention_shield_is_refused` returned above for both
        // classes. Kept as its own arm so the match stays wildcard-free, and
        // hard rather than `Ok(())` so a future route that bypasses the guard
        // cannot resolve a printed prevention into nothing (CR 611.2a: neither
        // a condition-bound nor an unsupported stated duration may be rewritten
        // as an end-of-turn shield).
        ReplacementDurationExpiry::GateControlled | ReplacementDurationExpiry::Unsupported => {
            return Err(EffectError::InvalidParam(
                "PreventDamage: unenforceable stated duration reached the shield builder \
                 (CR 611.2a)"
                    .to_string(),
            ));
        }
    }

    // CR 609.7 + CR 609.7a: "prevent that damage" from "a <color/type> source of
    // your choice" (Circle/Rune of Protection cycles) — the source is a player
    // choice. Unlike `create_damage_replacement::resolve`, this resolver had no
    // self-prompt path, so the choice was never offered. Prompt it now and
    // re-enter as a continuation; the recorded choice (with its qualifier stored
    // on `last_chosen_damage_source.source_filter`) is then resolved into a
    // durable `SpecificObject` + qualifier `And` shield by `resolve_source_filter`
    // below. A single `prompt_filter` drives both candidate enumeration and the
    // `WaitingFor` prompt so they cannot diverge.
    let effect_source_filter = match &effect_source_filter {
        Some(TargetFilter::ChosenDamageSource { filter: qualifier }) => {
            if state.last_chosen_damage_source.is_none() {
                let prompt_filter = qualifier.as_deref().cloned().unwrap_or(TargetFilter::Any);
                let options =
                    choose_damage_source::damage_source_options(state, ability, &prompt_filter);
                if !options.is_empty() {
                    state.park_ability_continuation(PendingContinuation::new(
                        Box::new(ability.clone()),
                        state,
                    ));
                    state.waiting_for = WaitingFor::DamageSourceChoice {
                        player: ability.controller,
                        source_filter: prompt_filter,
                        options,
                    };
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::PreventDamage,
                        source_id: ability.source_id,
                        subject: None,
                    });
                    return Ok(());
                }
                // CR 609.7a: no legal candidate — falls through with the record
                // still absent; the post-choice logic below then resolves
                // `resolve_source_filter`'s ChosenDamageSource arm against an empty
                // `last_chosen_damage_source`, producing a `TargetFilter::None`
                // shield that matches nothing (this activation does nothing).
                effect_source_filter.clone()
            } else {
                effect_source_filter.clone()
            }
        }
        other => other.clone(),
    };

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
    //
    // CR 615.5: AWE STRIKE — "You gain life equal to the damage prevented this
    // way" is a bare prevented-this-way rider (no when/whenever/if prelude). It
    // reaches this resolver as a `ContinuationStep` only for the one-shot
    // shape: the assembly gate (assembly.rs) forces `ContinuationStep` for the
    // bare rider only when the chain root's prevention carries the
    // `And{[ParentTargetSlot, Typed(creature)]}` source filter; for every other
    // chain root (e.g. Reverse Damage's `ChosenDamageSource` shape) the bare
    // rider stays a `SequentialSibling` and must NOT install here.
    //
    // The rider is installed via the SAME `runtime_execute` slot as every other
    // prevention rider — the resolution-time `ResolvedAbility` payload. The
    // applier resolves `EventContextAmount` in the rider against
    // `last_effect_count` (stamped with the prevented amount at apply time), so
    // no parse-time template reconstruction is needed; the whole sub-ability is
    // cloned verbatim, preserving every field the canonical
    // `build_resolved_from_def` converter round-trips.
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
                        // CR 611.2c + CR 613.1: install through the one
                        // resolution-install authority so the CR 613.1 layer reseed
                        // CARRIES this shield instead of wiping it. A prevention
                        // effect is not an object characteristic (CR 611.2c), so a
                        // layer pass has no authority to end it; a zone change
                        // (CR 400.7) and the `expiry` prunes still do.
                        obj.install_resolution_replacement(object_shield);
                    }
                }
                TargetRef::Player(player) => {
                    // Player-targeted prevention scopes to the chosen player and
                    // persists globally when created by an instant/sorcery on the stack.
                    let player_shield = shield
                        .clone()
                        .damage_target_filter(player_damage_filter(*player));
                    push_player_scoped_shield(
                        state,
                        ability.controller,
                        ability.source_id,
                        player_shield,
                    );
                }
            }
        }
    } else {
        // CR 113.7a + CR 611.2a + CR 615.3: Untargeted (SOURCE-scoped) prevention.
        // This branch used to fork on storage -- object-hosted when
        // `ability.source_id` was a permanent on the battlefield, registry-hosted
        // otherwise. That made the shield a characteristic of its source: the
        // CR 613.1 layer reset in `layers::seed_live_characteristics_from_base`
        // wiped it on the next pass, and the source leaving the battlefield ended an
        // effect that CR 113.7a says exists independently of it (issue #8485).
        // Every such shield now goes to the floating registry through the one
        // authority. `Zone::Battlefield` -- the exact predicate this branch used for
        // the old storage fork -- is passed as THIS caller's `anchor_zones`, so the
        // ANCHORED population is exactly the population this branch newly moves. An
        // untargeted shield sourced from a Command-zone object (an emblem) was
        // ALREADY registry-hosted here, so it stays unanchored and byte-identical to
        // its pre-#8485 behavior.
        //
        // CR 113.7a: a host-relative reference in the shield survives the move
        // because the authority latches the host IDENTITY on `source_object`, not
        // because the filter is rewritten. Both shapes this branch can produce are
        // covered: a `SelfRef` in `damage_source_filter` (the Mercenaries shape,
        // arriving through `resolve_source_filter`'s `_ => filter.clone()` fallback)
        // and a `SelfRef` in `valid_card` (the Gideon shape, through
        // `typed_recipient_valid_card_filter`'s `filter @ TargetFilter::SelfRef`
        // arm) are both resolved by the pending scan against that anchor, evaluating
        // the identical AST under an identical `FilterContext` to the object scan.
        crate::game::effects::install_floating_damage_replacement(
            state,
            shield,
            ability.controller,
            ability.source_id,
            &[Zone::Battlefield],
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PreventDamage,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 611.2a + CR 615: does the prevention-shield seam refuse this stated
/// lifetime? THE authority for that question, shared by two consumers:
///
/// * `resolve` above, which turns a refusal into a hard `EffectError` rather
///   than a successful no-op; and
/// * `parser::oracle::demote_unenforceable_replacement_lifetimes`, the
///   post-lowering honesty net, which demotes a refused line to
///   `Effect::Unimplemented` so no card reports the shape as supported.
///
/// The fallback order mirrors `resolve`'s exactly — the clause's own
/// `prevention_duration` first, the resolving ability's duration only when the
/// clause states none — so the net and the install seam cannot disagree about
/// which shape is supported.
///
/// Both refused classes are genuinely unenforceable HERE, for different
/// reasons: `Unsupported` has no expiry stamp at all, and `GateControlled`
/// promises a runtime applicability gate that only the bare untap-prevention
/// rider carries (`add_target_replacement::stamp_for_as_long_as_controlled_gate`)
/// — a prevention shield has no such gate, so it would install with no lifetime
/// the engine can end.
///
/// Reachable, measured over the parsed corpus: Old Fat Spider Can't See Me
/// chapter II states "for as long as this Saga remains on the battlefield" on a
/// prevention clause, which lands in the refused set. Before this guard it
/// resolved successfully and installed nothing.
pub(crate) fn prevention_shield_is_refused(
    prevention_duration: Option<&crate::types::ability::Duration>,
    ability_duration: Option<&crate::types::ability::Duration>,
) -> bool {
    use crate::game::effects::add_target_replacement::expiry_from_duration;
    let effective = match expiry_from_duration(prevention_duration) {
        ReplacementDurationExpiry::Unstated => expiry_from_duration(ability_duration),
        stated => stated,
    };
    matches!(
        effective,
        ReplacementDurationExpiry::GateControlled | ReplacementDurationExpiry::Unsupported
    )
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
                prevention_duration: None,
            },
            targets,
            source,
            PlayerId(0),
        )
    }

    /// CR 113.7a + CR 611.2a (issue #8485): an UNTARGETED (source-scoped)
    /// prevention shield goes to the floating registry, not onto the source
    /// permanent. Repointed from `prevent_all_creates_shield_on_source` and
    /// STRENGTHENED with the two install-time anchors — the storage location
    /// changed, no behavioral assertion was weakened.
    #[test]
    fn prevent_all_creates_a_floating_shield_anchored_to_its_source() {
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

        assert!(
            state
                .objects
                .get(&source)
                .unwrap()
                .replacement_definitions
                .is_empty(),
            "CR 113.7a: the shield must not ride on its source permanent"
        );
        assert_eq!(state.pending_damage_replacements.len(), 1);
        let shield = &state.pending_damage_replacements[0];
        assert!(matches!(
            shield.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(shield.event, ReplacementEvent::DamageDone);
        assert!(!shield.is_consumed);
        // CR 113.8: the installing controller is latched so a controller-relative
        // gate resolves under the sentinel host.
        assert_eq!(shield.source_controller, Some(PlayerId(0)));
        // CR 113.7a: the host identity this branch used to store the shield on is
        // carried on the definition instead.
        assert_eq!(shield.source_object, Some(source));
    }

    /// CR 511.2 + CR 615 (issue #2924, Bug B): a `prevention_duration` of
    /// `UntilEndOfCombat` ("this combat" — Suppressor Skyguard) must stamp the
    /// built shield with `RestrictionExpiry::EndOfCombat` so the EndCombat prune
    /// removes it and it does not bleed into a later combat the same turn.
    /// `UntilEndOfTurn` maps to `EndOfTurn`; `None` on BOTH carriers
    /// (`prevention_duration` here, and `ability.duration`, which
    /// `ResolvedAbility::new` leaves unset) falls to the engine's turn default in
    /// `ReplacementDefinition::with_resolution_shield_expiry` — an engine
    /// fallback, NOT a CR rule, since CR 611.2a's own no-duration case is "until
    /// the end of the game". That default is load-bearing: `turns::execute_cleanup`
    /// reads `expiry` alone, so a `None` here would make the shield immortal.
    #[test]
    fn prevention_duration_sets_shield_expiry() {
        use crate::types::ability::{Duration, RestrictionExpiry};

        let cases = [
            (
                Some(Duration::UntilEndOfCombat),
                Some(RestrictionExpiry::EndOfCombat),
            ),
            (
                Some(Duration::UntilEndOfTurn),
                Some(RestrictionExpiry::EndOfTurn),
            ),
            (None, Some(RestrictionExpiry::EndOfTurn)),
        ];
        for (duration, expected_expiry) in cases {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Suppressor Skyguard".to_string(),
                Zone::Battlefield,
            );
            let ability = ResolvedAbility::new(
                Effect::PreventDamage {
                    amount: PreventionAmount::All,
                    amount_dynamic: None,
                    target: TargetFilter::Controller,
                    scope: PreventionScope::CombatDamage,
                    damage_source_filter: None,
                    prevention_duration: duration.clone(),
                },
                vec![],
                source,
                PlayerId(0),
            );
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();

            // CR 113.7a (issue #8485): an untargeted shield from a battlefield
            // source now lives in the floating registry, anchored to that source.
            assert_eq!(state.pending_damage_replacements.len(), 1);
            assert_eq!(
                state.pending_damage_replacements[0].expiry, expected_expiry,
                "wrong shield expiry for prevention_duration {duration:?}"
            );
            assert_eq!(
                state.pending_damage_replacements[0].source_object,
                Some(source)
            );
        }
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
                prevention_duration: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 113.7a (issue #8485): untargeted => floating registry, anchored.
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert!(
            matches!(
                state.pending_damage_replacements[0].shield_kind,
                ShieldKind::Prevention {
                    amount: PreventionAmount::Next(4)
                }
            ),
            "dynamic Fixed(4) should resolve to a Next(4) shield, got {:?}",
            state.pending_damage_replacements[0].shield_kind
        );
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(source)
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
                damage_source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
                prevention_duration: None,
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

    // ---- Circle/Rune of Protection: "a <color/type> source of your choice" ----

    /// A `Typed` color qualifier matching objects whose color includes `color`.
    fn color_qualifier(color: ManaColor) -> TargetFilter {
        TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::HasColor { color }]))
    }

    /// A Circle/Rune of Protection prevention ability: "prevent that damage" from
    /// "a <qualifier> source of your choice". `qualifier: None` is the bare form.
    fn source_choice_prevent_ability(
        source: ObjectId,
        qualifier: Option<TargetFilter>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(TargetFilter::ChosenDamageSource {
                    filter: qualifier.map(Box::new),
                }),
                prevention_duration: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
    }

    /// Deal `amount` noncombat damage from `source` to player 0.
    fn deal_source_damage_to_p0(state: &mut GameState, source: ObjectId, amount: i32) {
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: amount },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(PlayerId(0))],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        deal_damage::resolve(state, &ability, &mut events).expect("damage resolves");
    }

    fn add_colored_source(
        state: &mut GameState,
        card: u64,
        owner: PlayerId,
        name: &str,
        color: ManaColor,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().color = vec![color];
        id
    }

    /// CR 609.7 + CR 609.7b: the PROMPT for "a red source of your choice" must
    /// offer ONLY red sources as legal choices. Reverting the resolver's new
    /// self-prompt block leaves `waiting_for` unchanged (no prompt), so the match
    /// arm panics — this is the primary discriminating assertion.
    #[test]
    fn circle_of_protection_red_prompt_options_are_color_filtered() {
        let mut state = GameState::new_two_player(42);
        let cop = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Circle of Protection: Red".to_string(),
            Zone::Battlefield,
        );
        let red = add_colored_source(&mut state, 2, PlayerId(1), "Red Source", ManaColor::Red);
        let blue = add_colored_source(&mut state, 3, PlayerId(1), "Blue Source", ManaColor::Blue);

        let ability = source_choice_prevent_ability(cop, Some(color_qualifier(ManaColor::Red)));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(options.contains(&red), "red source must be a legal choice");
                assert!(
                    !options.contains(&blue),
                    "blue source must NOT be offered for Circle of Protection: Red"
                );
            }
            other => panic!("expected DamageSourceChoice prompt, got {other:?}"),
        }
    }

    /// Sibling/negative: the BARE "a source of your choice" form (qualifier None)
    /// must offer BOTH the red and blue sources — proving the qualified and bare
    /// paths are genuinely distinguished, not both hard-filtered/unfiltered.
    #[test]
    fn bare_source_of_your_choice_prompt_offers_all_colors() {
        let mut state = GameState::new_two_player(42);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jade Monolith".to_string(),
            Zone::Battlefield,
        );
        let red = add_colored_source(&mut state, 2, PlayerId(1), "Red Source", ManaColor::Red);
        let blue = add_colored_source(&mut state, 3, PlayerId(1), "Blue Source", ManaColor::Blue);

        let ability = source_choice_prevent_ability(host, None);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(
                    options.contains(&red),
                    "bare form must offer the red source"
                );
                assert!(
                    options.contains(&blue),
                    "bare form must offer the blue source"
                );
            }
            other => panic!("expected DamageSourceChoice prompt, got {other:?}"),
        }
    }

    /// CR 609.7b + multi-authority: with TWO red sources present, the shield built
    /// after choosing one via the real `GameAction::ChooseDamageSource` pipeline
    /// prevents ONLY the chosen source's damage — the other red source's damage is
    /// dealt normally even though it also matches the color qualifier.
    #[test]
    fn circle_of_protection_red_prevents_only_chosen_red_source() {
        let mut state = GameState::new_two_player(42);
        let cop = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Circle of Protection: Red".to_string(),
            Zone::Battlefield,
        );
        let red1 = add_colored_source(&mut state, 2, PlayerId(1), "Red One", ManaColor::Red);
        let red2 = add_colored_source(&mut state, 3, PlayerId(1), "Red Two", ManaColor::Red);

        let ability = source_choice_prevent_ability(cop, Some(color_qualifier(ManaColor::Red)));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Drive the real choice through the engine resolution pipeline (this
        // exercises `engine_resolution_choices` + the pending-continuation
        // re-entry that builds the durable shield).
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseDamageSource { source: red1 },
        )
        .expect("submit damage source choice");

        // Chosen red source: damage prevented.
        deal_source_damage_to_p0(&mut state, red1, 3);
        assert_eq!(
            state.players[0].life, 20,
            "chosen red source's damage must be prevented"
        );
        // Other red source: damage NOT prevented (identity mismatch, CR 609.7b).
        deal_source_damage_to_p0(&mut state, red2, 3);
        assert_eq!(
            state.players[0].life, 17,
            "a different red source's damage must NOT be prevented"
        );
    }

    /// CR 609.7b: the shield rechecks the chosen source's live color at damage
    /// time. If the chosen source loses its red color before dealing damage, the
    /// shield does not apply (and, having never matched, is not consumed).
    #[test]
    fn recolored_chosen_source_defeats_color_qualified_shield() {
        let mut state = GameState::new_two_player(42);
        let cop = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Circle of Protection: Red".to_string(),
            Zone::Battlefield,
        );
        let red = add_colored_source(&mut state, 2, PlayerId(1), "Red Source", ManaColor::Red);

        let ability = source_choice_prevent_ability(cop, Some(color_qualifier(ManaColor::Red)));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseDamageSource { source: red },
        )
        .expect("submit damage source choice");
        // CR 113.7a (issue #8485): the untargeted shield lives in the floating
        // registry even though the source (Circle of Protection) is a battlefield
        // permanent — the effect exists independently of its source. Reach-guard:
        // prove the shield was actually installed, and that it carries the host
        // anchor so its host-relative gates still resolve.
        assert_eq!(
            state.pending_damage_replacements.len(),
            1,
            "shield must exist before the recheck"
        );
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(cop)
        );

        // CR 609.7b: chosen source becomes colorless before it deals damage.
        state.objects.get_mut(&red).unwrap().color = vec![];
        deal_source_damage_to_p0(&mut state, red, 3);
        assert_eq!(
            state.players[0].life, 17,
            "damage from a now-colorless source must NOT be prevented"
        );
        // CR 609.7b: a shield that never matched must not be consumed.
        assert!(
            !state.pending_damage_replacements[0].is_consumed,
            "a shield that never matched must not be consumed (CR 609.7b)"
        );
    }

    /// CR 609.7a: no legal source (no red objects anywhere) — no prompt fires and
    /// the ability resolves as a no-op shield that matches nothing; the game does
    /// not hang or error.
    #[test]
    fn circle_of_protection_red_no_legal_source_is_noop() {
        let mut state = GameState::new_two_player(42);
        let cop = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Circle of Protection: Red".to_string(),
            Zone::Battlefield,
        );
        let blue = add_colored_source(&mut state, 2, PlayerId(1), "Blue Source", ManaColor::Blue);

        let ability = source_choice_prevent_ability(cop, Some(color_qualifier(ManaColor::Red)));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::DamageSourceChoice { .. }),
            "no legal red source means no prompt should fire"
        );
        // The blue source's damage is not prevented (the no-op shield matches
        // nothing).
        deal_source_damage_to_p0(&mut state, blue, 3);
        assert_eq!(
            state.players[0].life, 17,
            "no-op shield must not prevent any damage"
        );
    }

    /// Rune of Protection: Lands exercises the TYPE-qualifier branch: the prompt
    /// must offer only Land sources, not a creature source.
    #[test]
    fn rune_of_protection_lands_prompt_options_are_type_filtered() {
        let mut state = GameState::new_two_player(42);
        let rune = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rune of Protection: Lands".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Damaging Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "A Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let land_qualifier = TargetFilter::Typed(TypedFilter::land());
        let ability = source_choice_prevent_ability(rune, Some(land_qualifier));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(
                    options.contains(&land),
                    "land source must be a legal choice"
                );
                assert!(
                    !options.contains(&creature),
                    "creature source must NOT be offered for Rune of Protection: Lands"
                );
            }
            other => panic!("expected DamageSourceChoice prompt, got {other:?}"),
        }
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

        // CR 113.7a (issue #8485): untargeted => floating registry, anchored.
        assert!(matches!(
            state.pending_damage_replacements[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::Next(3)
            }
        ));
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(source)
        );
    }

    /// CR 611.2a: a stated prevention window this seam cannot enforce must FAIL
    /// the resolution — installing nothing and reporting success is the defect.
    ///
    /// Printed member, measured over the parsed corpus: Old Fat Spider Can't See
    /// Me chapter II, "prevent all damage that would be dealt to … for as long as
    /// this Saga remains on the battlefield". The parse-time net
    /// (`parser::oracle::demote_unenforceable_replacement_lifetimes`) lowers that
    /// line to `Effect::Unimplemented` before it can reach here, so this test pins
    /// the SEAM rather than the pipeline: a route that bypasses the net must not
    /// be able to resolve a printed prevention into nothing.
    ///
    /// Both carriers CR 611.2a names are exercised — the ability-level window
    /// (`ability.duration`) and the effect grammar's own (`prevention_duration`) —
    /// because `prevention_shield_is_refused` reads them in that fallback order.
    /// Sibling of
    /// `add_target_replacement::tests::stated_unrepresentable_duration_does_not_install_a_shield`.
    #[test]
    fn stated_host_duration_does_not_install_a_prevention_shield() {
        use crate::types::ability::Duration;

        // Ability-level carrier, `Unsupported` class: the printed Old Fat Spider
        // wording. `WhileHostOnBattlefield` ends on phase-out (CR 702.26f), which
        // no expiry stamp this seam can build reproduces.
        let mut state = GameState::new_two_player(42);
        let saga = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Old Fat Spider Can't See Me".to_string(),
            Zone::Battlefield,
        );
        let mut ability = make_prevent_ability(
            saga,
            PreventionAmount::All,
            PreventionScope::AllDamage,
            vec![],
        );
        ability.duration = Some(Duration::WhileHostOnBattlefield);

        resolve(&mut state, &ability, &mut Vec::new()).expect_err(
            "CR 611.2a: an unenforceable stated prevention window must FAIL the \
             resolution, not succeed with no shield",
        );
        assert!(
            state.objects[&saga].replacement_definitions.is_empty(),
            "CR 611.2a: the refused window must not be shortened to the \
             end-of-turn fallback"
        );
        // Issue #8485: the untargeted branch now routes to the floating registry,
        // so the object-store negative alone would be VACUOUS. Both stores must be
        // empty for the refusal to mean anything.
        assert!(
            state.pending_damage_replacements.is_empty(),
            "CR 611.2a: the refused window must not install a floating shield either"
        );

        // Effect-level carrier, `GateControlled` class: the control wording earns
        // its gate only on the bare untap rider
        // (`add_target_replacement::stamp_for_as_long_as_controlled_gate`); a
        // prevention shield has no such gate, so it too is refused here.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Gated Prevention".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
                prevention_duration: Some(Duration::WhileControllingHost),
            },
            vec![],
            source,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut Vec::new()).expect_err(
            "CR 611.2a: a control-gated window on a shield that carries no gate \
             must FAIL the resolution",
        );
        assert!(
            state.objects[&source].replacement_definitions.is_empty(),
            "CR 611.2a: the refused window must not install an ungated shield"
        );
        assert!(
            state.pending_damage_replacements.is_empty(),
            "CR 611.2a: nor an ungated FLOATING shield (issue #8485)"
        );
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

        // CR 113.7a (issue #8485): untargeted => floating registry, anchored.
        assert_eq!(
            state.pending_damage_replacements[0].combat_scope,
            Some(CombatDamageScope::CombatOnly)
        );
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(source)
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
        let runtime = state.pending_damage_replacements[rid.index()]
            .runtime_execute
            .clone()
            .unwrap();
        state.last_effect_count = Some(prevented);
        state.install_ready_continuation(
            crate::types::ability::PostReplacementContinuation::Resolved(runtime),
        );
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
                prevention_duration: None,
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

    /// CR 615 + CR 201.5: a `SelfRef` recipient ("prevent all damage that would
    /// be dealt to HIM this turn" — Gideon Jura, Gideon of the Trials) scopes the
    /// shield to the SOURCE OBJECT.
    ///
    /// Two revert-failing halves, because the bug had two independent ways to
    /// manifest:
    ///   * `untargeted_damage_filter` must NOT lower `SelfRef` (a context ref) to
    ///     a PLAYER shield on the source's controller — that would prevent damage
    ///     to the Gideon's controller instead of to the Gideon.
    ///   * the shield must carry `valid_card: SelfRef` so it fires only on damage
    ///     to its host. With the pre-fix `TargetFilter::Any` recipient the shield
    ///     carried NO constraint at all and Fogged every damage event that turn —
    ///     which the negative half below catches.
    #[test]
    fn self_ref_recipient_prevention_scopes_the_shield_to_its_host() {
        let mut state = GameState::new_two_player(42);
        let gideon = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gideon Jura".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let bystander = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );
        for id in [gideon, bystander] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(6);
            obj.toughness = Some(6);
        }

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::SelfRef,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
                prevention_duration: None,
            },
            vec![],
            gideon,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 113.7a (issue #8485): the shield is source-scoped, so it lives in the
        // floating registry — but it stays OBJECT-scoped, because the host identity
        // travels with it on `source_object` and `valid_card: SelfRef` is evaluated
        // against that anchor rather than being rewritten. Repointed from the
        // pre-#8485 object-store assertion and strengthened with the anchor check;
        // the un-constrained global Fog placement (no `valid_card` at all) is still
        // excluded, now by the `valid_card` assertion below.
        assert!(
            state
                .objects
                .get(&gideon)
                .unwrap()
                .replacement_definitions
                .is_empty(),
            "CR 113.7a: the shield must not ride on its source permanent"
        );
        assert_eq!(state.pending_damage_replacements.len(), 1);
        let shield = &state.pending_damage_replacements[0];
        assert_eq!(
            shield.source_object,
            Some(gideon),
            "the SelfRef recipient filter resolves against this anchor"
        );
        assert_eq!(shield.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            shield.damage_target_filter, None,
            "SelfRef is an OBJECT recipient — lowering it to a player shield \
             would protect the controller instead of the Gideon"
        );

        let ctx = deal_damage::DamageContext::from_source(&state, damage_source).unwrap();
        // Damage to the host is prevented.
        let to_host = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(gideon),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(to_host, deal_damage::DamageResult::Applied(0)),
            "damage to the shield's host is prevented"
        );

        // Negative half: everything else still takes damage. This is what fails
        // on the pre-fix `Any` recipient, which prevented all damage in the game.
        let to_bystander = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(bystander),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(to_bystander, deal_damage::DamageResult::Applied(3)),
            "another permanent is untouched by a host-scoped shield"
        );
        let to_controller = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(0)),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(to_controller, deal_damage::DamageResult::Applied(3)),
            "the source's CONTROLLER is not the recipient"
        );
        assert_eq!(state.players[0].life, 17);
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
                prevention_duration: None,
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
                prevention_duration: None,
            },
            vec![],
            pack_leader,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 113.7a (issue #8485): untargeted => floating registry, anchored.
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(pack_leader)
        );
        let shield = &state.pending_damage_replacements[0];
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

    /// CR 608.2c + CR 611.2c + CR 615.11 (issue #6682): A `TrackedSet(0)`
    /// sentinel recipient must be resolved to a CONCRETE tracked-set id at
    /// shield-creation time, not left as the raw sentinel. Proves the
    /// staleness bug this resolves: without the fix, a persisting shield's
    /// `valid_card: TrackedSet(0)` would be re-resolved against
    /// `state.chain_tracked_set_id` at EVERY future damage check, drifting to
    /// whatever unrelated chain most recently published a tracked set. Here,
    /// AFTER the shield is created, an unrelated chain overwrites
    /// `chain_tracked_set_id` to point at a completely different object
    /// (simulating some other spell resolving later the same turn) — the
    /// shield must still protect only the ORIGINAL untapped creature (Energy
    /// Arc class), not the new unrelated set.
    #[test]
    fn tracked_set_recipient_resolves_to_concrete_id_immune_to_later_chain_overwrite() {
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Energy Arc".to_string(),
            Zone::Stack,
        );
        let untapped_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Untapped Creature".to_string(),
            Zone::Battlefield,
        );
        let unrelated_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Unrelated Creature".to_string(),
            Zone::Battlefield,
        );
        let damage_source = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        // The preceding Untap clause published the untapped creature as the
        // chain's tracked set — the state `prevent_damage::resolve` sees.
        state
            .tracked_object_sets
            .insert(TrackedSetId(5), vec![untapped_creature]);
        state.chain_tracked_set_id = Some(TrackedSetId(5));

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                scope: PreventionScope::CombatDamage,
                damage_source_filter: None,
                prevention_duration: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // A LATER, unrelated chain publishes a fresh tracked set (e.g. some
        // other spell's exile/mill effect resolving afterward this turn).
        state
            .tracked_object_sets
            .insert(TrackedSetId(6), vec![unrelated_creature]);
        state.chain_tracked_set_id = Some(TrackedSetId(6));

        let ctx = deal_damage::DamageContext::from_source(&state, damage_source).unwrap();

        // The original untapped creature must still be protected.
        let protected_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(untapped_creature),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(protected_result, deal_damage::DamageResult::Applied(0)),
            "the shield must stay bound to the originally-untapped creature"
        );

        // The later, unrelated creature must NOT be protected — the shield
        // must not have drifted onto whatever the newest tracked set is.
        let unrelated_result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(unrelated_creature),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(unrelated_result, deal_damage::DamageResult::Applied(3)),
            "the shield must NOT drift onto an unrelated later chain's tracked set"
        );
    }

    /// CR 611.2c + CR 615.11 (issue #6682): Mutational Advantage's official
    /// ruling — "The set of permanents affected by Mutational Advantage is
    /// determined at the time Mutational Advantage resolves. Permanents that
    /// gain counters later in the turn won't become affected by this effect,
    /// and permanents that lose all of their counters later in the turn
    /// won't stop being affected." — driven through the real cast pipeline
    /// (`GameRunner::cast`), not a hand-built `ResolvedAbility`, so the parse
    /// → chain-context → resolution path is exercised end to end.
    ///
    /// Setup: `countered` already has a +1/+1 counter (in the frozen
    /// population); `uncountered` does not. AFTER the spell resolves:
    /// `uncountered` gains a counter (must NOT retroactively join the
    /// shielded population — a live re-check of "permanents with counters"
    /// would wrongly protect it) and `countered` loses its counter (must
    /// STAY protected — the shield is bound to the frozen object identity,
    /// not a live filter re-evaluated at each damage event).
    #[test]
    fn mutational_advantage_shield_freezes_population_at_resolution() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::counter::CounterType;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use std::sync::Arc;

        let def = parse_effect_chain(
            "Permanents you control with counters on them gain hexproof and indestructible \
             until end of turn. Prevent all damage that would be dealt to those permanents \
             this turn.",
            AbilityKind::Spell,
        );

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let countered = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Countered Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&countered).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.counters.insert(CounterType::Plus1Plus1, 1);
        }
        let uncountered = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Uncountered Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&uncountered)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let spell = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Mutational Advantage".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(def);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
                generic: 1,
            };
        }
        for color in [ManaType::Green, ManaType::Blue, ManaType::Colorless] {
            state.players[0].mana_pool.add(ManaUnit {
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

        let mut runner = crate::game::scenario::GameRunner::from_state(state);
        let _outcome = runner.cast(spell).resolve();
        let state = runner.state_mut();

        // CR 611.2c: mutate AFTER resolution — the frozen population must be
        // immune to both changes.
        state
            .objects
            .get_mut(&uncountered)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state
            .objects
            .get_mut(&countered)
            .unwrap()
            .counters
            .remove(&CounterType::Plus1Plus1);

        let attacker = create_object(
            state,
            CardId(4),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let ctx = deal_damage::DamageContext::from_source(state, attacker).unwrap();
        let mut events = Vec::new();

        let uncountered_result = deal_damage::apply_damage_to_target(
            state,
            &ctx,
            TargetRef::Object(uncountered),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(uncountered_result, deal_damage::DamageResult::Applied(3)),
            "a permanent that gains a counter AFTER resolution must NOT retroactively \
             join the frozen shielded population"
        );

        let countered_result = deal_damage::apply_damage_to_target(
            state,
            &ctx,
            TargetRef::Object(countered),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(countered_result, deal_damage::DamageResult::Applied(0)),
            "a permanent that loses its counter AFTER resolution must STAY protected \
             (frozen by identity, not re-checked live)"
        );
    }

    /// CR 608.2c + CR 615 (issue #6682): Energy Arc's bidirectional "dealt to
    /// and dealt by those creatures" — driven through the real cast pipeline
    /// (`GameRunner::cast`) with a genuine SUBSET target selection out of two
    /// eligible creatures, proving the shield scopes to exactly the SELECTED
    /// creature in BOTH directions:
    /// - combat damage dealt TO the selected creature is prevented; TO the
    ///   nonselected creature is not.
    /// - combat damage dealt BY the selected creature (as a source) is
    ///   prevented; BY the nonselected creature is not.
    #[test]
    fn energy_arc_cast_pipeline_scopes_to_and_by_damage_to_selected_creature() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use std::sync::Arc;

        let def = parse_effect_chain(
            "Untap any number of target creatures. Prevent all combat damage that would \
             be dealt to and dealt by those creatures this turn.",
            AbilityKind::Spell,
        );

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Two eligible creatures — only one is selected as Energy Arc's
        // target, proving the shield scopes to the CHOSEN subset, not every
        // creature the multi-target filter could have matched.
        let selected = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Selected Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&selected)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state.objects.get_mut(&selected).unwrap().tapped = true;

        let nonselected = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Nonselected Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&nonselected)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state.objects.get_mut(&nonselected).unwrap().tapped = true;

        let opponent_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let spell = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Energy Arc".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(def);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::Blue],
                generic: 0,
            };
        }
        for color in [ManaType::White, ManaType::Blue] {
            state.players[0].mana_pool.add(ManaUnit {
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

        let mut runner = crate::game::scenario::GameRunner::from_state(state);
        // CR 601.2c: "any number of target creatures" — declare exactly ONE
        // of the two eligible creatures, proving the shield binds to the
        // CHOSEN subset (the driver matches declared object intent to the
        // multi-target slot).
        let _outcome = runner.cast(spell).target_objects(&[selected]).resolve();
        let state = runner.state_mut();

        assert!(
            !state.objects.get(&selected).unwrap().tapped,
            "the selected creature must be untapped by Energy Arc's own effect"
        );

        let opponent_attacker_ctx =
            deal_damage::DamageContext::from_source(state, opponent_creature).unwrap();
        let mut events = Vec::new();

        // Damage TO: selected is shielded, nonselected is not.
        let to_selected = deal_damage::apply_damage_to_target(
            state,
            &opponent_attacker_ctx,
            TargetRef::Object(selected),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(to_selected, deal_damage::DamageResult::Applied(0)),
            "combat damage dealt TO the selected creature must be prevented"
        );
        let to_nonselected = deal_damage::apply_damage_to_target(
            state,
            &opponent_attacker_ctx,
            TargetRef::Object(nonselected),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(to_nonselected, deal_damage::DamageResult::Applied(3)),
            "combat damage dealt TO the nonselected creature must NOT be prevented"
        );

        // Damage BY: selected as the source is shielded, nonselected as the
        // source is not.
        let selected_source_ctx = deal_damage::DamageContext::from_source(state, selected).unwrap();
        let by_selected = deal_damage::apply_damage_to_target(
            state,
            &selected_source_ctx,
            TargetRef::Object(opponent_creature),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(by_selected, deal_damage::DamageResult::Applied(0)),
            "combat damage dealt BY the selected creature must be prevented"
        );
        let nonselected_source_ctx =
            deal_damage::DamageContext::from_source(state, nonselected).unwrap();
        let by_nonselected = deal_damage::apply_damage_to_target(
            state,
            &nonselected_source_ctx,
            TargetRef::Object(opponent_creature),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(by_nonselected, deal_damage::DamageResult::Applied(3)),
            "combat damage dealt BY the nonselected creature must NOT be prevented"
        );
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
                prevention_duration: None,
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
                prevention_duration: None,
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
            // CR 113.7a (issue #8485): untargeted => floating registry.
            let shield = &state.pending_damage_replacements[0];
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
            // CR 113.7a (issue #8485): untargeted => floating registry.
            let shield = &state.pending_damage_replacements[0];
            assert!(
                shield.runtime_execute.is_none(),
                "a SequentialSibling sub must NOT install as runtime_execute"
            );
        }
    }

    // ---- Issue #8485: CR 113.7a source-independence + the host-identity anchor ----

    /// Install an untargeted (source-scoped) prevention shield from `source`.
    fn install_untargeted_shield(
        state: &mut GameState,
        source: ObjectId,
        controller: PlayerId,
        target: TargetFilter,
        damage_source_filter: Option<TargetFilter>,
        amount: PreventionAmount,
    ) {
        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount,
                amount_dynamic: None,
                target,
                scope: PreventionScope::AllDamage,
                damage_source_filter,
                prevention_duration: None,
            },
            vec![],
            source,
            controller,
        );
        resolve(state, &ability, &mut Vec::new()).expect("prevention resolves");
    }

    /// Deal `amount` noncombat damage from `source` to `target`, returning the
    /// amount that actually landed. Drives the real replacement pipeline.
    fn damage_landed(
        state: &mut GameState,
        source: ObjectId,
        target: TargetRef,
        amount: u32,
    ) -> u32 {
        let ctx = deal_damage::DamageContext::from_source(state, source).expect("damage context");
        let mut events = Vec::new();
        match deal_damage::apply_damage_to_target(state, &ctx, target, amount, false, &mut events)
            .expect("damage resolves")
        {
            deal_damage::DamageResult::Applied(n) => n,
            deal_damage::DamageResult::NeedsChoice => {
                panic!("unexpected CR 616.1 replacement-choice park")
            }
        }
    }

    /// CR 113.7a (issue #8485, BL1): a HOST-RELATIVE damage-source filter must keep
    /// matching after the shield is routed to the floating registry.
    ///
    /// Mercenaries — "{3}: The next time this creature would deal damage to you
    /// this turn, prevent that damage." — lowers to `damage_source_filter:
    /// Some(TargetFilter::SelfRef)` and takes the untargeted branch. Under the bare
    /// `ObjectId(0)` sentinel, `filter.rs`'s `object_matches_trigger_source` would
    /// compare the damage source against `ObjectId(0)` and never match, silently
    /// deleting the shield. The `source_object` anchor supplies the host identity
    /// the sentinel cannot, WITHOUT rewriting the filter — so the pending scan
    /// evaluates the identical AST under an identical `FilterContext` to the object
    /// scan.
    ///
    /// Revert-failing against A2(i)'s anchor threading: drop `source_host` from the
    /// `damage_source_filter` context and leg (ii) fails.
    #[test]
    fn mercenaries_shaped_selfref_source_shield_still_matches_after_routing() {
        let mut state = GameState::new_two_player(42);
        let mercenaries = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mercenaries".to_string(),
            Zone::Battlefield,
        );
        let twin = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mercenaries".to_string(),
            Zone::Battlefield,
        );

        install_untargeted_shield(
            &mut state,
            mercenaries,
            PlayerId(0),
            TargetFilter::Any,
            Some(TargetFilter::SelfRef),
            PreventionAmount::All,
        );

        // (i) Reach-guard: exactly one registry entry, carrying the host anchor.
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(mercenaries),
            "CR 113.7a: the host identity must travel with the shield"
        );

        // (ii) Damage dealt BY that permanent is prevented.
        assert_eq!(
            damage_landed(&mut state, mercenaries, TargetRef::Player(PlayerId(0)), 3),
            0,
            "\"this creature\" as the damage source must still resolve to the anchor"
        );

        // (iii) HOSTILE: a second, identical permanent's damage is NOT prevented.
        // This is what separates the anchor from a blanket shield: the filter is
        // evaluated, not ignored.
        assert_eq!(
            damage_landed(&mut state, twin, TargetRef::Player(PlayerId(0)), 3),
            3,
            "a different Mercenaries' damage must not be prevented by this shield"
        );
    }

    /// CR 109.1 (issue #8485, BL1 second instance): the "you and OTHER permanents
    /// you control" exclusion is HOST-RELATIVE and must survive routing.
    ///
    /// `untargeted_damage_filter` lowers
    /// `TargetFilter::ControllerAndControlledPermanents { source_scope: Exclude }`
    /// into `DamageTargetFilter::PlayerOrPermanentsControlledBy { source_scope:
    /// Exclude }`, whose permanent leg is `*oid != repl_source`
    /// (`replacement.rs`). Under the sentinel that comparison is inert — the shield
    /// would start protecting its OWN host, the exact inverse of the printed "other"
    /// article. Revert-failing against A2(i)'s `matches_damage_target_filter` anchor.
    #[test]
    fn other_permanents_exclusion_survives_routing() {
        let mut state = GameState::new_two_player(42);
        let wanderer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Wanderer".to_string(),
            Zone::Battlefield,
        );
        let ally = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ally".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        install_untargeted_shield(
            &mut state,
            wanderer,
            PlayerId(0),
            TargetFilter::ControllerAndControlledPermanents {
                permanent_type: None,
                source_scope: crate::types::ability::SourceExclusion::Exclude,
            },
            None,
            PreventionAmount::All,
        );
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(wanderer)
        );

        // Reach-guard: another permanent that player controls IS protected.
        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Object(ally), 3),
            0,
            "the shield must protect other permanents its controller controls"
        );
        // CR 109.1: the HOST itself is excluded by the "other" article.
        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Object(wanderer), 3),
            3,
            "CR 109.1: \"OTHER permanents you control\" must not cover the source"
        );
    }

    /// CR 109.1 + CR 614.1a (issue #8485, migration hazard; settles U3): the pending
    /// scan's `valid_card` gate now delegates to `replacement_valid_card_matches` —
    /// the same authority the per-object scan uses — so the two agree for every
    /// `ProposedEvent::Damage` shape, including the player-target case where they
    /// previously diverged.
    ///
    /// A "prevent all damage that would be dealt to creatures this turn" shield
    /// carries `valid_card: Some(Typed(creature))` and `damage_target_filter: None`.
    /// Before the consolidation the pending scan SKIPPED the gate for a player
    /// recipient, so moving such a shield to the registry would newly have made it
    /// prevent damage dealt to players. This also corrects the pre-existing
    /// instant-sourced (Blinding Fog class) case.
    #[test]
    fn pending_valid_card_gate_matches_the_object_path() {
        let mut state = GameState::new_two_player(42);
        let fog = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Blinding Fog".to_string(),
            Zone::Battlefield,
        );
        let bear = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        install_untargeted_shield(
            &mut state,
            fog,
            PlayerId(0),
            TargetFilter::Typed(TypedFilter::creature()),
            None,
            PreventionAmount::All,
        );
        let shield = &state.pending_damage_replacements[0];
        assert_eq!(
            shield.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature()))
        );
        assert_eq!(shield.damage_target_filter, None);

        // Positive reach-guard: object damage matching the filter IS prevented.
        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Object(bear), 3),
            0
        );
        // CR 109.1: a player is not an object, so player damage is NOT prevented.
        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Player(PlayerId(0)), 3),
            3,
            "CR 109.1: a card-shaped recipient filter must not cover a player"
        );
    }

    /// CR 113.7a (issue #8485, MG-A; settles U7): the `source_object` anchor is
    /// latched ONLY over each caller's OWN pre-existing object-hosting zone set, so
    /// it is inert for every shield that was already registry-hosted before it
    /// existed. Four legs.
    ///
    /// Legs (iii) and (iv) are the multi-authority pair: the SAME Command-zone
    /// source anchors through `push_player_scoped_shield` (whose `anchor_zones` is
    /// `[Battlefield, Command]`, the pair its old storage fork tested) but does NOT
    /// anchor through `resolve`'s untargeted branch (whose `anchor_zones` is
    /// `[Battlefield]` alone, the predicate ITS old fork tested). Revert-failing
    /// against A3(b)'s per-caller slice: a single hardcoded `Battlefield | Command`
    /// test would flip leg (iii) to `Some`, newly activating `SourceExclusion::
    /// Exclude` and `DamageTargetPlayerScope::SourceChosenPlayer` on a population
    /// that is already registry-hosted today — over-matching, the exact inverse of
    /// the under-matching the anchor exists to fix.
    #[test]
    fn instant_sourced_shield_carries_no_source_object_anchor() {
        // (i) A shield installed from a STACK source (instant mid-resolution).
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Stack,
        );
        install_untargeted_shield(
            &mut state,
            spell,
            PlayerId(0),
            TargetFilter::Any,
            None,
            PreventionAmount::All,
        );
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_object, None,
            "a shield created by a resolving instant never had a host to anchor"
        );
        // CR 113.8: the controller is latched regardless.
        assert_eq!(
            state.pending_damage_replacements[0].source_controller,
            Some(PlayerId(0))
        );

        // (ii) Paired positive reach-guard: a BATTLEFIELD source does anchor, so
        // leg (i) is not passing merely because the anchor path never fires.
        let mut state = GameState::new_two_player(42);
        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Circle of Protection".to_string(),
            Zone::Battlefield,
        );
        install_untargeted_shield(
            &mut state,
            permanent,
            PlayerId(0),
            TargetFilter::Any,
            None,
            PreventionAmount::All,
        );
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(permanent)
        );

        // (iii) HOSTILE (MG-A): a COMMAND-zone source (an emblem) routed through
        // `resolve`'s untargeted branch. That branch's old storage fork tested
        // `Zone::Battlefield` ALONE, so a Command-zone source was ALREADY going to
        // the registry unanchored — and must stay that way.
        let mut state = GameState::new_two_player(42);
        let emblem = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Emblem".to_string(),
            Zone::Command,
        );
        install_untargeted_shield(
            &mut state,
            emblem,
            PlayerId(0),
            TargetFilter::Any,
            None,
            PreventionAmount::All,
        );
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_object, None,
            "an emblem-sourced untargeted shield was already registry-hosted, so \
             anchoring it would be an unmeasured behavior change"
        );

        // (iv) MULTI-AUTHORITY: the SAME Command-zone source, routed instead
        // through `push_player_scoped_shield` (a PLAYER-targeted prevention), DOES
        // anchor — because that caller genuinely hosted on Battlefield | Command.
        let mut state = GameState::new_two_player(42);
        let emblem = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Emblem".to_string(),
            Zone::Command,
        );
        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Player,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
                prevention_duration: None,
            },
            vec![TargetRef::Player(PlayerId(0))],
            emblem,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut Vec::new()).expect("prevention resolves");
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(emblem),
            "`push_player_scoped_shield`'s own zone set includes Command"
        );
        // Incidental fix pinned: the old registry arm of `push_player_scoped_shield`
        // never latched `source_controller` (CR 113.8); the authority always does.
        assert_eq!(
            state.pending_damage_replacements[0].source_controller,
            Some(PlayerId(0))
        );
    }

    /// CR 702.26b + CR 113.7a (issue #8485, M6): a source-scoped shield survives its
    /// SOURCE phasing out. CR 702.26b makes a phased-out PERMANENT nonexistent; it
    /// says nothing about a continuous effect that already exists, and CR 113.7a
    /// says the effect is independent of its source once the ability resolved. A
    /// Circle of Protection whose controller phases it out mid-turn must not lose
    /// the shield it already created.
    ///
    /// This is a DELIBERATE behavior change: object-hosted definitions are filtered
    /// through `functioning_abilities::object_functions`, which returns false for a
    /// phased-out permanent; the floating registry has no such gate.
    #[test]
    fn floating_shield_survives_its_source_phasing_out() {
        let mut state = GameState::new_two_player(42);
        let cop = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Circle of Protection".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        install_untargeted_shield(
            &mut state,
            cop,
            PlayerId(0),
            TargetFilter::Any,
            None,
            PreventionAmount::All,
        );

        // Positive reach-guard: the shield applies BEFORE the phase-out.
        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Player(PlayerId(0)), 2),
            0
        );

        state.objects.get_mut(&cop).unwrap().phase_status =
            crate::game::game_object::PhaseStatus::PhasedOut {
                cause: crate::game::game_object::PhaseOutCause::Directly,
            };
        assert!(state.objects[&cop].is_phased_out());

        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Player(PlayerId(0)), 2),
            0,
            "CR 113.7a: the effect exists independently of its phased-out source"
        );
    }

    /// CR 616.1 (issue #8485; settles U2): two applicable prevention shields on one
    /// damage event — one object-hosted, one in the floating registry — prevent the
    /// damage exactly ONCE, with no panic and no double-consume.
    ///
    /// OBSERVED VERDICT (this is the U2 measurement the plan asked to record, and it
    /// contradicts the guess this test was first written with): the engine PARKS.
    /// `apply_damage_to_target` returns `DamageResult::NeedsChoice` and
    /// `state.waiting_for` becomes `WaitingFor::ReplacementChoice`, because CR 616.1
    /// gives the AFFECTED player the choice of which applicable replacement to apply
    /// first and `replacement_ordering_is_material` finds the order material for two
    /// prevention shields on one damage event. It does NOT auto-resolve.
    ///
    /// That matters for Unit A specifically: the pending scan runs AFTER the
    /// per-object walk in `find_applicable_replacements`, so moving a shield from
    /// the object store to the registry changes the ORDER the two candidates are
    /// offered in — and `PendingReplacement.candidates` parks those raw
    /// `ReplacementId`s across a layer flush, which is exactly why Unit B's carried
    /// entries are settled to the TAIL rather than inserted ahead of derived grants.
    ///
    /// The choice is submitted through the real production path
    /// (`GameAction::ChooseReplacement` via `apply_as_current`), not by poking the
    /// pipeline, so this covers the `WaitingFor` route rather than a helper.
    #[test]
    fn two_shields_on_one_damage_event_prevent_it_exactly_once() {
        let mut state = GameState::new_two_player(42);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Shield Source".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        // Shield 1: object-hosted on the damage recipient (the targeted arm).
        // Both shields are `Next(1)` against a 1-damage event: a `Prevention { All }`
        // shield mutates nothing when it applies (it stays live for the rest of the
        // turn), so it could not make "exactly once" observable at all. `Next(1)`
        // depletes, and 1 damage is fully absorbed by the first shield to apply, so
        // the CR 616.1 loop ends before the second becomes applicable again.
        let targeted = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::Next(1),
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
                prevention_duration: None,
            },
            vec![TargetRef::Object(host)],
            source,
            PlayerId(0),
        );
        resolve(&mut state, &targeted, &mut Vec::new()).expect("targeted shield resolves");
        // Shield 2: source-scoped, in the floating registry (the untargeted arm).
        install_untargeted_shield(
            &mut state,
            source,
            PlayerId(0),
            TargetFilter::Any,
            None,
            PreventionAmount::Next(1),
        );

        // Reach-guard: both stores really hold one shield each, so the CR 616.1
        // competition below is not vacuous.
        assert_eq!(state.objects[&host].replacement_definitions.len(), 1);
        assert_eq!(state.pending_damage_replacements.len(), 1);
        let object_before = format!("{:?}", state.objects[&host].replacement_definitions.first());
        let registry_before = format!("{:?}", state.pending_damage_replacements.first());

        let ctx = deal_damage::DamageContext::from_source(&state, attacker).expect("context");
        let result = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(host),
            1,
            false,
            &mut Vec::new(),
        )
        .expect("damage resolves");
        assert!(
            matches!(result, deal_damage::DamageResult::NeedsChoice),
            "U2 observation: the engine PARKS a CR 616.1 replacement choice here"
        );
        let WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected a ReplacementChoice park, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            player,
            PlayerId(0),
            "CR 616.1: the AFFECTED object's controller chooses"
        );
        assert_eq!(candidate_count, 2, "both shields are offered");

        // Submit the choice through the real action path.
        state.priority_player = player;
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseReplacement { index: 0 },
        )
        .expect("submit the CR 616.1 replacement choice");

        assert_eq!(
            state.objects[&host].damage_marked, 0,
            "the damage is prevented"
        );
        // CR 614.5: exactly ONE of the two shields absorbs the event — compared by
        // whole-definition Debug so the assertion holds whether the applier records
        // the use as `is_consumed` or as a decremented amount.
        let object_changed =
            format!("{:?}", state.objects[&host].replacement_definitions.first()) != object_before;
        let registry_changed =
            format!("{:?}", state.pending_damage_replacements.first()) != registry_before;
        assert!(
            object_changed != registry_changed,
            "CR 614.5: exactly one shield may absorb one damage event \
             (object_changed={object_changed}, registry_changed={registry_changed})"
        );
    }

    /// CR 616.1 + CR 113.7a (issue #8485, R1): the CR 616.1 replacement-choice
    /// PROMPT must name the permanent that created a registry-hosted shield.
    ///
    /// This is a regression THIS change would otherwise have introduced. Before
    /// #8485 a Maze of Ith / Circle of Protection / Mercenaries shield was
    /// object-hosted, so `ReplacementCandidateSummary` read `name_of(rid.source)`
    /// and got the permanent's name. Unit A moves those shields into
    /// `state.pending_damage_replacements`, whose `rid.source` is the `ObjectId(0)`
    /// storage sentinel — which per CR 109.4 has no entry in `state.objects`, so the
    /// name went blank and the description fell through to the bare "Replacement
    /// effect" placeholder. That lands exactly where this change also makes CR 616.1
    /// prompts newly appear (see
    /// `two_shields_on_one_damage_event_prevent_it_exactly_once`), so a player could
    /// be asked to choose between two indistinguishable options.
    ///
    /// `replacement_choice_display_source` / `replacement_choice_definition` in
    /// `replacement.rs` resolve the DISPLAY anchor through the shield's CR 113.7a
    /// `source_object`. `ReplacementId::source` is untouched as a storage
    /// discriminator — `handle_replacement_choice` still resolves the raw `rid`.
    ///
    /// Revert-failing: revert either helper and the registry candidate's
    /// `source_id` is `ObjectId(0)` with an empty `source_name`.
    #[test]
    fn replacement_choice_prompt_names_the_anchored_source_of_a_registry_shield() {
        /// Build the two-competing-shields park and return the parked candidates.
        /// `shield_source_zone` decides whether the registry shield is anchored:
        /// `Battlefield` anchors it (issue #8485's moved population), `Stack` does
        /// not (a resolving instant never had a host).
        fn park_with_two_shields(
            shield_source_zone: Zone,
        ) -> (
            GameState,
            ObjectId,
            ObjectId,
            Vec<crate::types::game_state::ReplacementCandidateSummary>,
        ) {
            let mut state = GameState::new_two_player(42);
            let host = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Shielded Creature".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&host).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                obj.power = Some(2);
                obj.toughness = Some(2);
            }
            let shield_source = create_object(
                &mut state,
                CardId(2),
                PlayerId(0),
                "Circle of Protection".to_string(),
                shield_source_zone,
            );
            let attacker = create_object(
                &mut state,
                CardId(3),
                PlayerId(1),
                "Attacker".to_string(),
                Zone::Battlefield,
            );

            // Object-hosted shield on the recipient (the targeted arm).
            let targeted = ResolvedAbility::new(
                Effect::PreventDamage {
                    amount: PreventionAmount::Next(1),
                    amount_dynamic: None,
                    target: TargetFilter::Any,
                    scope: PreventionScope::AllDamage,
                    damage_source_filter: None,
                    prevention_duration: None,
                },
                vec![TargetRef::Object(host)],
                shield_source,
                PlayerId(0),
            );
            resolve(&mut state, &targeted, &mut Vec::new()).expect("targeted shield resolves");
            // Registry-hosted shield (the untargeted arm).
            install_untargeted_shield(
                &mut state,
                shield_source,
                PlayerId(0),
                TargetFilter::Any,
                None,
                PreventionAmount::Next(1),
            );
            assert_eq!(state.pending_damage_replacements.len(), 1);

            let ctx = deal_damage::DamageContext::from_source(&state, attacker).expect("context");
            let result = deal_damage::apply_damage_to_target(
                &mut state,
                &ctx,
                TargetRef::Object(host),
                1,
                false,
                &mut Vec::new(),
            )
            .expect("damage resolves");
            assert!(
                matches!(result, deal_damage::DamageResult::NeedsChoice),
                "reach-guard: two applicable shields must park a CR 616.1 choice"
            );
            let WaitingFor::ReplacementChoice { candidates, .. } = state.waiting_for.clone() else {
                panic!(
                    "expected a ReplacementChoice park, got {:?}",
                    state.waiting_for
                );
            };
            assert_eq!(candidates.len(), 2, "both shields are offered");
            (state, host, shield_source, candidates)
        }

        // POSITIVE: a battlefield source anchors, so the prompt names it.
        let (state, host, cop, candidates) = park_with_two_shields(Zone::Battlefield);
        assert_eq!(
            state.pending_damage_replacements[0].source_object,
            Some(cop),
            "reach-guard: this is the anchored (moved) population"
        );
        let registry_option = candidates
            .iter()
            .find(|c| c.source_id == cop)
            .unwrap_or_else(|| {
                panic!(
                    "CR 616.1: the registry shield's option must name its host, got {candidates:?}"
                )
            });
        assert_eq!(
            registry_option.source_name, "Circle of Protection",
            "CR 113.7a: the shield's host identity travels with it and names the option"
        );
        // The label site now reads the REGISTRY entry, so the option describes
        // itself instead of falling through to the bare placeholder.
        assert_eq!(
            Some(registry_option.description.clone()),
            state.pending_damage_replacements[0].description.clone(),
            "the option's description must come from the registry entry itself"
        );
        // SIBLING: the object-hosted option is unaffected and still names its host.
        let object_option = candidates
            .iter()
            .find(|c| c.source_id == host)
            .expect("the object-hosted shield's option still names its host");
        assert_eq!(object_option.source_name, "Shielded Creature");

        // NEGATIVE SIBLING: a STACK-sourced shield has no host to anchor, so it
        // degrades to today's behavior — the sentinel and an empty name — rather
        // than panicking or naming some unrelated object.
        let (state, host, _spell, candidates) = park_with_two_shields(Zone::Stack);
        assert_eq!(
            state.pending_damage_replacements[0].source_object, None,
            "an instant-sourced shield carries no anchor"
        );
        let unanchored = candidates
            .iter()
            .find(|c| c.source_id == ObjectId(0))
            .unwrap_or_else(|| {
                panic!("the unanchored option keeps the sentinel, got {candidates:?}")
            });
        assert_eq!(
            unanchored.source_name, "",
            "no anchor means no name — it must not borrow another object's"
        );
        assert!(
            candidates.iter().filter(|c| c.source_id == host).count() == 1,
            "the unanchored option must not be mislabelled as the object-hosted one"
        );
    }

    /// CR 113.8 + CR 109.5 + CR 611.2a (issue #8485, MG-B): a controller-relative
    /// gate on a MOVED shield follows the INSTALLER, not the source permanent's live
    /// controller.
    ///
    /// This is the second identity axis Unit A changes. The object scan computes
    /// `replacement_source_player(obj)` — the host's LIVE controller, re-read every
    /// pass — and never reads `source_controller`; the pending scan uses
    /// `source_controller.unwrap_or(active_player)`, which the install authority now
    /// always latches. CR 113.8: "The controller of an activated ability on the
    /// stack is the player who activated it." CR 109.5: for an activated ability,
    /// "you" is the player who activated the ability. CR 611.2a: the continuous
    /// effect lasts as stated by the ability that created it, so its "you" is fixed
    /// at resolution and does not follow the source permanent to a new controller.
    ///
    /// HOSTILE FIXTURE: the control change is the only input that separates the two
    /// readings, and `state.active_player` is set to the NON-installer so the
    /// `unwrap_or(active_player)` fallback is also discriminated.
    /// Revert-failing against A3(a): without the latch, `source_controller` is
    /// `None` and the gate drifts to whoever is active.
    #[test]
    fn controller_relative_gate_follows_the_installer_not_the_live_host_controller() {
        let mut state = GameState::new_two_player(42);
        // The NON-installer is active, so `unwrap_or(state.active_player)` would
        // answer P1 if the latch were missing.
        state.active_player = PlayerId(1);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Comeuppance".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        install_untargeted_shield(
            &mut state,
            source,
            PlayerId(0),
            TargetFilter::ControllerAndControlledPermanents {
                permanent_type: None,
                source_scope: crate::types::ability::SourceExclusion::Include,
            },
            None,
            PreventionAmount::All,
        );
        // Reach-guard, and the pin that this IS the moved population.
        let shield = &state.pending_damage_replacements[0];
        assert_eq!(shield.source_controller, Some(PlayerId(0)));
        assert_eq!(shield.source_object, Some(source));
        assert!(matches!(
            shield.damage_target_filter,
            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::Controller,
                ..
            })
        ));

        // CR 611.2c: change control of the SOURCE permanent (Threaten / Ray of
        // Command) inside the shield's window.
        state.objects.get_mut(&source).unwrap().controller = PlayerId(1);

        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Player(PlayerId(0)), 3),
            0,
            "CR 113.8: the shield still protects the player who activated the ability"
        );
        assert_eq!(
            damage_landed(&mut state, attacker, TargetRef::Player(PlayerId(1)), 3),
            3,
            "the source's NEW controller does not inherit the shield"
        );
    }
}

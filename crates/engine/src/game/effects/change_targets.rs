use crate::game::ability_utils;
use crate::game::ability_utils::{RetargetSlotBinding, SlotEnforcement};
use crate::game::targeting;
use crate::game::targeting::find_legal_targets;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, RetargetScope, RetargetSlotAddress, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::ObjectId;

/// CR 115.7: Change the target(s) of a spell or ability on the stack.
///
/// Resolves in two modes:
/// - `forced_to` is `Some`: directly update the stack entry's targets to the resolved target.
/// - `forced_to` is `None`: set `WaitingFor::RetargetChoice` so the player selects the new target.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ChangeTargets {
        target,
        scope,
        forced_to,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "ChangeTargets effect missing".to_string(),
        ));
    };

    // CR 115.7 + CR 608.2k: the retarget subject may be a DECLARED target
    // ("target spell") or a CONTEXT REF ("that spell" on a spell-cast trigger —
    // Perplexing Chimera's `TriggeringSource`), which surfaces no target slot.
    // Both are bound by the single 4-tier authority `targeting::resolved_targets`,
    // whose chosen-targets tier preserves the prior declared-target behavior
    // (ability.targets[0] is still the TargetRef::Object(id) of the stack entry
    // being retargeted for a declared subject).
    // CR 115.7 (Class D, OUT OF RUN): a non-`you` chooser ("the spell's
    // controller may choose new targets") needs a chooser slot on
    // `Effect::ChangeTargets`; the chooser here is `ability.controller`.
    let stack_entry_id = targeting::resolved_targets(ability, target, state)
        .into_iter()
        .find_map(|t| match t {
            TargetRef::Object(id) => Some(id),
            TargetRef::Player(_) => None,
        })
        .ok_or_else(|| {
            EffectError::MissingParam("ChangeTargets requires a stack entry target".into())
        })?;

    // CR 115.7: Find the stack entry by its object ID.
    let stack_entry_index = state
        .stack
        .iter()
        .position(|e| e.id == stack_entry_id)
        .ok_or_else(|| {
            EffectError::MissingParam("ChangeTargets: targeted entry not on stack".to_string())
        })?;

    let Some(stack_ability) = state.stack[stack_entry_index].ability().cloned() else {
        // Permanent spell with no ability — nothing to retarget.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    // CR 115.7d vs CR 115.7a/CR 115.7b: only "choose new targets" (`All`) is an
    // operation over the WHOLE target set, and only its submission
    // (`GameAction::RetargetSpell.new_targets`) is index-aligned with that set,
    // so only it can address a slot below the root. "Change the target(s)" /
    // "change a target" (`Single`) submits ONE bare `TargetRef` through
    // `GameAction::ChooseTarget` with no slot index, and its projection asks
    // for exactly one pick — there is no way for the player to say WHICH
    // target is being changed. Widening its offer would hand it candidates the
    // submission provably cannot write, which is the unanswerable-prompt shape
    // `retarget_prompt_softlock.rs` exists to prevent. `Single`/`ForcedTo`
    // therefore keep BASE's exposure exactly: the BASE-exposed prefix, which
    // `chain_retarget_slots` guarantees equals `stack_ability.targets` (equal
    // in VALUE under `AdditionalCostPaidInstead` delegation, where the root
    // mirrors the delegated sub).
    //
    // SCOPE SELECTS THE EXPOSED PREFIX, NOT A POOL AUTHORITY. Every exposed
    // position — under every scope — gets its pool from the same one
    // computation (`slot_pool`, INVARIANT SC). Round 5 gave `Single` a
    // different pool authority from its enforcement and they went disjoint on
    // a printed card (Hallow: BASE offers 4 targets none of which is CR-legal
    // for a "target spell" slot, and refuses both that are —
    // phase-rs/phase#8355 round-5 defect B9).
    //
    // Scope also decides ANSWERABILITY, which is a different question from
    // authority and is asked separately, after the pools exist
    // (`retarget_prompt_is_dischargeable`). Narrowing `Single`'s admit set to
    // `slot_pools[0]` without moving the dischargeability guard out of the
    // flat index space parks a prompt nothing can discharge — measured on
    // Hallow with its declared target spell removed from the stack
    // (phase-rs/phase#8355 round-6 defect B10).
    let bindings = ability_utils::chain_retarget_slots(&stack_ability);
    let exposed: &[RetargetSlotBinding] = match scope {
        RetargetScope::All => &bindings[..],
        RetargetScope::Single | RetargetScope::ForcedTo(_) => base_exposed_prefix(&bindings),
    };
    let current_targets: Vec<TargetRef> = exposed.iter().map(|b| b.current.clone()).collect();
    let slots: Vec<RetargetSlotAddress> = exposed.iter().map(|b| b.address.clone()).collect();
    debug_assert!(
        stack_ability.targets.is_empty() || !exposed.is_empty(),
        "CR 115.7: a non-empty `current_targets` must expose >=1 position"
    );

    if current_targets.is_empty() {
        // CR 115.7: Retargeting changes existing targets of the target spell or
        // ability. A stack entry with no current targets has no retarget choice
        // to make, so the effect resolves as a no-op rather than opening an
        // impossible selection state.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 109.5 + INVARIANT SC: the same pool_controller / slot_pool
    // computation the interactive path uses, called a second time here
    // because the forced path returns before the prompt would be constructed
    // (Invariant SC, `chain_retarget_slots`' doc) — this and the interactive
    // path below are its TWO call sites, both in this file.
    let base = legal_new_targets_for_entry(state, &state.stack[stack_entry_index]);
    let pool_controller =
        retarget_pool_controller(state, &state.stack[stack_entry_index], &stack_ability);
    let slot_pools: Vec<Vec<TargetRef>> = exposed
        .iter()
        .map(|b| {
            let node =
                ability_utils::node_at(&stack_ability, &b.address.path).unwrap_or(&stack_ability);
            slot_pool(state, node, &b.enforcement, pool_controller, &base)
        })
        .collect();

    if let Some(filter) = forced_to {
        // CR 115.7a/b: Forced retarget — resolve the new target from the filter,
        // but only apply it if the targeted stack entry could legally target it.
        let new_targets = find_legal_targets(state, filter, ability.controller, ability.source_id);
        if let Some(new_target) = new_targets.into_iter().find(|target| base.contains(target)) {
            // CR 115.7b: "change a target" replaces exactly ONE of the targeted
            // stack entry's declared positions — the FIRST exposed position
            // whose slot pool admits the candidate AND whose current target
            // actually differs from it (CR 115.7a: a change to itself is not a
            // change). Generalizes the old `mana_multi_role`-only scan to
            // every exposed position, second call site of Invariant SC.
            if let Some(i) =
                forced_retarget_target_position(exposed, &slot_pools, &current_targets, &new_target)
            {
                write_retarget_position(state, stack_entry_index, &exposed[i].address, &new_target);
            }
            // CR 115.7a: no exposed position can legally change to another
            // target -> every target is left unchanged.
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // Interactive retarget: present choices to the player.
    // CR 115.7a: The current targets of the targeted spell/ability become the starting point.
    // CR 115.7: Enumerate legal new targets by re-evaluating the stack entry's
    // own targeting restriction against the current game state.
    //
    // CR 303.4a: An Aura SPELL's target is defined by its enchant *ability*, not
    // by its effect's target field — the synthesized spell ability carries a
    // placeholder effect with no targetable filter (`target_filter()` is `None`),
    // so Aura hosts are enumerated from the source's `Keyword::Enchant(filter)`
    // instead, mirroring the Aura branch of `casting::spell_has_legal_targets`.
    // CR 115.1b: that substitution is keyed on the STACK ENTRY being the Aura
    // spell — "An Aura permanent doesn't target anything; only the spell is
    // targeted. (An activated or triggered ability of an Aura permanent can also
    // be targeted.)" Every other entry — including a triggered or activated
    // ability whose source happens to be a resident Aura — falls back to its own
    // effect's declared target filter.
    //
    // CR 115.7d, INVARIANT SC: the UNION is BASE's cascade EXTENDED, never
    // replaced, so it stays a literal prefix of BASE's (Invariant B) and the
    // dischargeability gate below cannot newly fire where BASE's `:155` guard
    // did not. Extend-if-absent rather than concatenate: a root position's
    // pool now frequently EQUALS the cascade, and blind concatenation would
    // double the list the projection renders. BASE's own internal duplicates
    // are preserved untouched — do NOT deduplicate `base` itself.
    let mut legal_new_targets = base.clone();
    for p in &slot_pools {
        for t in p {
            if !legal_new_targets.contains(t) {
                legal_new_targets.push(t.clone());
            }
        }
    }

    // CR 115.7a: "If a target can't be changed to another legal target, the
    // original target is unchanged, even if the original target is itself
    // illegal by then." An unanswerable prompt IS that case, so there is no
    // choice to make. Parking anyway produces a prompt nothing can discharge:
    // `apply_retarget`'s `Single` arm requires membership in the ADDRESSED
    // POSITION'S pool (INVARIANT SC) and has no unchanged-position exemption,
    // and `interaction.rs`'s projection asks for N picks from that prompt's
    // candidates. Resolve as a no-change instead — mirroring the empty-
    // `current_targets` no-op guard above.
    //
    // THIS GUARD MUST BE ASKED IN THE INDEX SPACE ADMISSION USES. At
    // `bb28b0e8b` admission was membership in the flat cascade, so
    // `legal_new_targets.is_empty()` was the whole question. Under INVARIANT
    // SC admission is per position, and a `Single` prompt whose position 0 has
    // an empty pool is unanswerable even though the UNION is not empty —
    // measured on Hallow whose declared target spell left the stack
    // (phase-rs/phase#8355 round-6 defect B10). For a `Legacy` position this
    // predicate degenerates to `!base.is_empty()`, i.e. to `bb28b0e8b`'s test
    // exactly; for `All` it IS `bb28b0e8b`'s test, because `All` always admits
    // the no-change submission (CR 115.7d).
    if !retarget_prompt_is_dischargeable(scope, &slot_pools, &legal_new_targets) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::RetargetChoice {
        player: ability.controller,
        stack_entry_index,
        scope: scope.clone(),
        current_targets,
        slots,
        slot_pools,
        legal_new_targets,
    };
    // EffectResolved is emitted by the engine handler after RetargetSpell action is submitted.
    Ok(())
}

/// CR 115.7a: a parked `RetargetChoice` must be DISCHARGEABLE — at least one
/// submission `engine::apply_retarget` accepts must exist. This is the SAME
/// question the flat `legal_new_targets.is_empty()` guard asked at
/// `bb28b0e8b`; it is asked here in the index space admission now uses
/// (INVARIANT SC: position `i` is admitted by `slot_pools[i]`, nothing else).
///
/// "If a target can't be changed to another legal target, the original target
/// is unchanged, even if the original target is itself illegal by then."
/// (CR 115.7a). An addressed position with an empty pool IS that case, so the
/// effect resolves as a no-change rather than parking a prompt with no answer.
///
/// `pub(crate)` (phase-rs/phase#8355 round-8 review finding H1, second pass):
/// `resolve` is not this predicate's only caller. `engine::apply_retarget` and
/// `ai_support::candidates::retarget_actions` re-derive per-position pools for
/// an outer-empty compat payload (`derive_slot_pools`, H3) whose per-position
/// legality can disagree with the payload's OWN `legal_new_targets` (a
/// `Single` position can re-derive to an empty pool while the stored union is
/// non-empty — measured on a B10-shaped Hallow board). Neither call site ran
/// this gate before using the re-derived pools, so a re-derived-undischargeable
/// position was admitted nothing, including its own unchanged current target,
/// with no fallback: an unconditional hang. Both call sites now ask this
/// SAME question of the re-derived pools before trusting them, exactly as
/// `resolve` asks it before parking; when it says no, they fall back to
/// `legal_new_targets` — the field's own doc's promise "behaves as at BASE".
pub(crate) fn retarget_prompt_is_dischargeable(
    scope: &RetargetScope,
    slot_pools: &[Vec<TargetRef>],
    legal_new_targets: &[TargetRef],
) -> bool {
    match scope {
        // `Single` writes position 0 and ONLY position 0 (`apply_retarget`'s
        // `Single` arm requires `new_targets.len() == 1`). Its admit set is
        // `slot_pools[0]`, with NO unchanged-position exemption — CR 115.7a/b
        // make "change a target" mandatory where an alternative exists, so the
        // exemption `All` has under CR 115.7d must NOT be granted here.
        // (TRACKED(pending-approval) #12: admission itself is unconditioned on
        // `changes`, so a pool-member current target de facto declines a
        // change; see `apply_retarget`'s `Single` arm.)
        RetargetScope::Single => slot_pools.first().is_some_and(|p| !p.is_empty()),
        // CR 115.7d: `All` always admits the no-change submission (every
        // position takes `apply_retarget`'s unchanged-position skip), so it is
        // dischargeable whenever there is anything to RENDER. That is the
        // union — `bb28b0e8b`'s `:155` test, preserved verbatim.
        RetargetScope::All => !legal_new_targets.is_empty(),
        // Unreachable here: `RetargetScope::ForcedTo` has NO construction site
        // anywhere in the workspace (the parser emits only `Single`/`All`).
        // `false` is the fail-safe that agrees with `apply_retarget`, which
        // rejects a `ForcedTo` submission unconditionally: such a prompt is
        // undischargeable by definition, so resolving as no-change is
        // strictly better than parking it.
        RetargetScope::ForcedTo(_) => false,
    }
}

/// CR 115.7a/115.7b: `Single`/`ForcedTo` may change only a target the
/// operation itself exposes, which is the BASE-exposed node's own slots. THE
/// single definition of "the BASE-exposed prefix"; the exposure gate above,
/// the forced path above and row P-NO-LEGACY-SUB all ask this one question.
///
/// Correct exactly where `bindings` came from `chain_retarget_slots` on an
/// entry past the `current_targets.is_empty()` guard: exactly one node is
/// emitted with `base_exposed = true` (emitted first, so its bindings are the
/// contiguous front of the vector); no descended node can share its path
/// (descent strictly grows paths); and a node with non-empty `targets` always
/// emits >=1 binding, so `bindings[0]` is always a BASE-exposed binding at
/// this seam.
fn base_exposed_prefix(bindings: &[RetargetSlotBinding]) -> &[RetargetSlotBinding] {
    let n = bindings.first().map_or(0, |first| {
        bindings
            .iter()
            .take_while(|b| b.address.path == first.address.path)
            .count()
    });
    &bindings[..n]
}

/// CR 115.7a + CR 115.7b: Determine which addressed slot (if any) a forced
/// single-target retarget's candidate legally changes. Mirrors
/// `ability_utils::retarget_slot_violation`'s "changes && legal" conjunction on
/// slot identity — the FIRST exposed position whose slot pool admits the
/// candidate AND whose current target actually differs from it. CR 115.7a: "If
/// a target can't be changed to another legal target, the original target is
/// unchanged" — if no slot qualifies, `None`.
///
/// SINGLE-POSITION CASE (`exposed.len() == 1`, the overwhelming majority of
/// forced retargets — BASE's `None => Some(0)` fallback for a non-mana-role
/// node): NOT gated on `changes`. `write_retarget_position`'s own
/// `retarget_target_requires_pin_refresh` call is what decides whether a
/// write is a genuine change OR a same-TargetRef re-incarnation (CR 400.7 +
/// CR 603.7c) that still needs its pin refreshed; gating candidacy on raw
/// `TargetRef` inequality here would make that same-ID case unreachable, since
/// `forced_retarget_target_position` has no incarnation context to tell the
/// two apart. A MULTI-position node (mana `Both`) keeps the `changes` gate,
/// matching BASE's `Some(role)` branch, which never had this single-slot
/// special case to begin with.
fn forced_retarget_target_position(
    exposed: &[RetargetSlotBinding],
    slot_pools: &[Vec<TargetRef>],
    current_targets: &[TargetRef],
    new_target: &TargetRef,
) -> Option<usize> {
    if exposed.len() == 1 {
        return slot_pools
            .first()
            .is_some_and(|p| p.contains(new_target))
            .then_some(0);
    }
    (0..exposed.len()).find(|&i| {
        let changes = current_targets.get(i).is_some_and(|cur| cur != new_target);
        let legal = slot_pools.get(i).is_some_and(|p| p.contains(new_target));
        changes && legal
    })
}

/// CR 115.7d: write a single addressed position's new target, refreshing its
/// target-incarnation pin (CR 400.7 + CR 603.7c) and re-deriving the chain's
/// non-declared targets (`restamp_derived_chain_targets`). Shared per-address
/// writer for the forced path here and `engine::apply_retarget`'s interactive
/// write loop, so the two cannot disagree about what "write position `i`"
/// means.
fn write_retarget_position(
    state: &mut GameState,
    stack_entry_index: usize,
    address: &RetargetSlotAddress,
    new_target: &TargetRef,
) {
    let Some(mut mutated) = state.stack[stack_entry_index].ability().cloned() else {
        return;
    };
    if let Some(node) = ability_utils::node_at_mut(&mut mutated, &address.path) {
        if let Some(old) = node.targets.get(address.slot).cloned() {
            let refresh = node.retarget_target_requires_pin_refresh(&old, new_target, state);
            node.targets[address.slot] = new_target.clone();
            if refresh {
                let pin = match new_target {
                    TargetRef::Object(id) => {
                        state.objects.get(id).map(ObjectIncarnationRef::from_object)
                    }
                    TargetRef::Player(_) => None,
                };
                if let Some(pin) = pin {
                    node.update_selected_target_incarnation(pin);
                }
            }
        }
    }
    ability_utils::restamp_derived_chain_targets(&mut mutated);
    if let Some(stack_ability_mut) = state.stack[stack_entry_index].ability_mut() {
        *stack_ability_mut = mutated;
    }
}

/// CR 109.5 (+ CR 400.7a for a permanent spell whose controller changed):
/// during the CR 115.7d window, "you" in this entry's filters is the spell's
/// CURRENT controller — the live object row, not `ResolvedAbility.controller`,
/// which is still the caster until `stack::resolve_top` re-stamps after the
/// window closes. Extracted VERBATIM from the cascade's own expression so the
/// cascade and every per-position pool read one authority. Falls back to
/// `stack_ability.controller` for a triggered/activated entry with no
/// `state.objects` row (CR 113.7a).
///
/// THIS IS THE SEAM'S ONLY CONTROLLER SOURCE. No other player value may be
/// passed to a legal-set producer anywhere in the retarget path
/// (phase-rs/phase#8355 round-5 defect B8). `pub(crate)` so
/// `engine.rs::apply_retarget`'s CR 115.7d second-clause pass (Step 2.6b) can
/// obtain the value by CALLING it rather than by receiving it on the payload —
/// its call sites are enumerated by row V-CTRL(2), not bounded by privacy.
pub(crate) fn retarget_pool_controller(
    state: &GameState,
    entry: &StackEntry,
    stack_ability: &ResolvedAbility,
) -> PlayerId {
    state
        .objects
        .get(&entry.id)
        .map_or(stack_ability.controller, |obj| obj.controller)
}

/// CR 115.7d + INVARIANT SC: THE candidate set for ONE addressed position.
/// This is the ONLY expression in the engine that produces one at an addressed
/// position. Its value is stored in `WaitingFor::RetargetChoice::slot_pools[i]`;
/// `apply_retarget`, `retarget_slot_violation`, `forced_retarget_target_position`
/// and `ai_support::candidates::retarget_actions` all READ that stored vector
/// and none derives another.
///
/// `find_legal_targets_for_ability_with_controller` (`targeting.rs:57`) is the
/// one constructor that can serve both roles: it carries the addressed NODE
/// (so a filter's node-relative predicates resolve against the node that
/// declares it) AND an explicit controller (CR 109.5 + CR 400.7a). Also CR
/// 115.1 + CR 702.11b + CR 702.16b + CR 702.18a: that controller drives
/// `can_target`'s hexproof / protection / shroud checks under the spell's
/// CURRENT controller, not the caster.
///
/// NO root/sub branch and NO scope branch. Both are position-independent
/// facts, and treating either as a pool concept is what produced round-4
/// defect B7 and round-5 defect B9. Module-private — its call sites are both
/// in this file: `resolve`'s single shared prompt/forced-path computation
/// (the interactive and forced branches read ONE `slot_pools` vector computed
/// before they split, so this is one call site serving both), and
/// `derive_slot_pools` (H3, below — the outer-empty-payload re-derivation
/// `engine::apply_retarget` and `ai_support::candidates::retarget_actions`
/// call). This is what INVARIANT SC's compile-enforced half rests on (row
/// V-ONE-SITES): no OTHER module can call `slot_pool` itself, whatever calls
/// through it.
fn slot_pool(
    state: &GameState,
    node: &ResolvedAbility,
    enforcement: &SlotEnforcement,
    pool_controller: PlayerId,
    base: &[TargetRef],
) -> Vec<TargetRef> {
    match enforcement {
        // No per-slot authority exists here, so BASE's cascade IS this
        // position's authority — cloned verbatim (review-round3's Invariant C;
        // round-3 defect B6).
        SlotEnforcement::Legacy => base.to_vec(),
        // `f` is the whole authority for this slot, so its own legal set IS
        // the candidate set.
        SlotEnforcement::Filtered(f) => targeting::find_legal_targets_for_ability_with_controller(
            state,
            f,
            node,
            pool_controller,
        ),
    }
}

/// CR 115.7d + INVARIANT SC + N16 (phase-rs/phase#8355 round-8 review finding
/// H3): reconstruct real per-position pools for a `#[serde(default)]` payload
/// whose OUTER `slot_pools` is empty (a payload predating the field, or a
/// version-skewed persisted state). Calls the SAME one expression (`slot_pool`)
/// the interactive and forced sites use, so such a payload gets the identical
/// per-position enforcement a live prompt would have stored, instead of
/// `apply_retarget`'s / `retarget_actions`' `pool_for`/`retarget_slot_violation`
/// degrading EVERY position to the flat union.
///
/// That flat-union degrade is not merely weaker — it is exactly round-5 defect
/// B2's shape reopened: `retarget_slot_violation` no longer takes `&GameState`
/// (Invariant SC), so a filter like `PreventDamage`'s source slot or a
/// mana-role's per-role split can no longer be re-derived INSIDE the
/// validator, and pool membership against the union admits candidates a real
/// per-position pool would reject. Re-deriving here, at the two call sites
/// that DO have `&GameState` (`engine::apply_retarget`,
/// `ai_support::candidates::retarget_actions`), closes that gap without
/// widening `slot_pool`'s own visibility.
///
/// `pub(crate)` — its callers are outside this file (unlike `slot_pool`
/// itself, which stays module-private); the caller passes `bindings` FRESHLY
/// derived from the live stack entry (`chain_retarget_slots`), not the
/// prompt's stale `slots` addresses, so the returned pools are index-aligned
/// with that fresh derivation.
pub(crate) fn derive_slot_pools(
    state: &GameState,
    entry: &StackEntry,
    stack_ability: &ResolvedAbility,
    bindings: &[RetargetSlotBinding],
) -> Vec<Vec<TargetRef>> {
    let base = legal_new_targets_for_entry(state, entry);
    let pool_controller = retarget_pool_controller(state, entry, stack_ability);
    bindings
        .iter()
        .map(|b| {
            let node =
                ability_utils::node_at(stack_ability, &b.address.path).unwrap_or(stack_ability);
            slot_pool(state, node, &b.enforcement, pool_controller, &base)
        })
        .collect()
}

/// Extract the target filter from an effect variant, if it has a standard `target` field.
/// Used to compute legal alternative targets for retargeting (CR 115.7).
fn extract_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    effect.target_filter()
}

/// CR 115.7: Enumerate the legal replacement targets for a spell or ability on
/// the stack by re-evaluating that stack entry's own target restriction against
/// current game state. Shared by interactive retargets, forced retargets, and AI
/// policy scoring so they cannot disagree about what can be changed to what.
pub fn legal_new_targets_for_stack_entry(
    state: &GameState,
    stack_entry_index: usize,
) -> Vec<TargetRef> {
    state
        .stack
        .get(stack_entry_index)
        .map(|entry| legal_new_targets_for_entry(state, entry))
        .unwrap_or_default()
}

fn legal_new_targets_for_entry(state: &GameState, entry: &StackEntry) -> Vec<TargetRef> {
    let Some(stack_ability) = entry.ability() else {
        return Vec::new();
    };

    // CR 115.7 + CR 608.2c: enumerate the replacement pool against the spell's
    // CURRENT controller, not the caster. Perplexing Chimera's printed ruling is
    // explicit: "The change of control happens before new targets are chosen, so
    // any targeting restrictions such as 'target opponent' or 'target creature
    // you control' are now made in reference to you, not the spell's original
    // controller." `ResolvedAbility.controller` is still the caster at this
    // point — the exchange installs a layer-2 `ChangeController` on the OBJECT,
    // and the stack entry's ability is only re-stamped later, in
    // `stack::resolve_top`, by which time the retarget window has closed. So the
    // pool must come from the object, exactly as the other stack-time
    // controller readers this branch introduced already do
    // (`derived_views`, `casting::targets_commit_crime`,
    // `ability_utils::parent_target_controller`).
    //
    // Falls back to `stack_ability.controller` rather than
    // `stack::stack_object_controller`'s `entry.controller`: a triggered or
    // activated ability entry has a freshly-allocated id with no `state.objects`
    // row (CR 113.7a), and the ability's own controller is the authority there.
    // For a spell entry the object row always exists, so the live value wins.
    //
    // Extracted into `retarget_pool_controller` — THE SEAM'S ONLY CONTROLLER
    // SOURCE — so the cascade and every per-position pool (`slot_pool`) read
    // one expression (row V-CTRL(2)).
    let pool_controller = retarget_pool_controller(state, entry, stack_ability);

    // CR 303.4a: "An Aura spell requires a target, which is defined by its
    // enchant ability." That is a statement about the Aura SPELL — the object on
    // the stack whose resolution puts the Aura onto the battlefield — and it is
    // needed here only because the cast path synthesizes a placeholder spell
    // ability whose `target_filter()` is `None`.
    //
    // CR 115.1b + CR 113.7a: A triggered or activated ability of an Aura already
    // on the battlefield is a DIFFERENT object on the stack — 113.7a, "once
    // activated or triggered, an ability exists on the stack independently of its
    // source" — and 115.1b says outright that "an Aura permanent doesn't target
    // anything; only the spell is targeted. (An activated or triggered ability of
    // an Aura permanent can also be targeted.)" So it declares its own target
    // through its own effect (Pain for All: "When this Aura enters, enchanted
    // creature deals damage equal to its power to any other target").
    // Keying this branch on "the source object is an Aura" instead of on the
    // stack entry claimed those abilities too and handed back the Aura's
    // "creature you control" enchant pool for them — a pool that cannot even
    // contain the ability's current target, so every retarget submission was
    // rejected and no actor could discharge the prompt.
    if matches!(entry.kind, StackEntryKind::Spell { .. }) {
        if let Some(filter) = aura_enchant_filter(state, stack_ability.source_id) {
            return find_legal_targets(state, &filter, pool_controller, stack_ability.source_id);
        }
    }

    // CR 115.7 + CR 601.2c: A multi-role mana declares its recipient AND its
    // count source as independent instances of "target"; both are legally
    // retargetable. The standard branch below reads `Effect::target_filter()`,
    // which returns only the FIRST DECLARED role filter and RETURNS
    // UNCONDITIONALLY — so it must not run first here, or the second role would
    // be silently unretargetable. Build the pool over ALL surfaced role filters
    // instead. Placed like the Aura branch above for the same reason: this
    // node's real target restriction is not the one the generic accessor
    // reports.
    //
    // The pool is necessarily FLAT (`Vec<TargetRef>`, no slot structure), so it
    // is a SUPERSET pre-filter for the UI/AI. Per-slot CR 115.7a legality is
    // enforced at the assignment seam by `retarget_slot_violation`, consulted by
    // BOTH `engine.rs::apply_retarget` and the AI generator
    // (`ai_support::candidates::retarget_actions`) so the two cannot propose and
    // reject different sets. Single-role manas take
    // `mana_multi_role == None` and are served entirely by the standard branch,
    // exactly as before.
    if let Some(role) = crate::types::ability::mana_multi_role(&stack_ability.effect) {
        let options: Vec<TargetRef> = role
            .surfaced_filters()
            .flat_map(|(_slot, filter)| {
                find_legal_targets(state, filter, pool_controller, stack_ability.source_id)
            })
            .collect();
        if !options.is_empty() {
            return options;
        }
    }

    // CR 115.7: Standard targeted spell/ability — re-evaluate its own declared
    // target filter against current game state.
    if let Some(filter) = extract_target_filter(&stack_ability.effect) {
        return find_legal_targets(state, filter, pool_controller, stack_ability.source_id);
    }

    // CR 109.4: A mass effect that targets a player via a population filter
    // ("tap all creatures target player controls", "destroy all artifacts that
    // player controls") surfaces a player target slot, yet its
    // `Effect::target_filter()` is `None` (the field is a resolution-time scan,
    // not a targeting filter), so the standard branch above can't reach it.
    // Enumerate the legal replacement *players* via the same companion-slot
    // authority the cast path uses so retargeting offers a real alternative
    // instead of collapsing to the current target.
    if let Some(players) =
        crate::game::ability_utils::companion_target_player_retarget_options(state, stack_ability)
    {
        return players;
    }

    // CR 115.7a: No declared or derived target filter (e.g. a placeholder spell
    // effect) — keep the current targets unchanged.
    stack_ability.targets.clone()
}

/// CR 303.4a: An Aura spell's legal targets are defined by its enchant ability —
/// modeled here as the source object's `Keyword::Enchant(filter)` — not by its
/// (placeholder) spell effect. Returns that filter when `source_id` is an Aura,
/// so retargeting an Aura spell (CR 115.7) enumerates the permanents it could
/// legally enchant. Mirrors the Aura branch of `casting::spell_has_legal_targets`.
pub(crate) fn aura_enchant_filter(state: &GameState, source_id: ObjectId) -> Option<TargetFilter> {
    let obj = state.objects.get(&source_id)?;
    if !obj.card_types.subtypes.iter().any(|s| s == "Aura") {
        return None;
    }
    obj.keywords.iter().find_map(|k| match k {
        Keyword::Enchant(filter) => Some(filter.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{TypeFilter, TypedFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{CastingVariant, RetargetScope, StackEntry, StackEntryKind};
    use crate::types::identifiers::CardId;

    /// P-GATE — `retarget_prompt_is_dischargeable` degenerates to `bb28b0e8b`'s
    /// flat `:155` test in both degenerate cases, and is unconditionally
    /// `false` for the unreachable `ForcedTo` arm. Each arm is exercised with
    /// BOTH verdicts, so no arm passes by being constantly true or false.
    #[test]
    fn retarget_prompt_is_dischargeable_degenerates_to_the_flat_test() {
        // (a) `Single` at a `Legacy` position: the verdict equals
        // `!base.is_empty()` — here, `slot_pools[0] == base`, since a `Legacy`
        // position's pool IS the cascade.
        assert!(retarget_prompt_is_dischargeable(
            &RetargetScope::Single,
            &[vec![TargetRef::Player(PlayerId(0))]],
            &[TargetRef::Player(PlayerId(0))],
        ));
        assert!(!retarget_prompt_is_dischargeable(
            &RetargetScope::Single,
            &[vec![]],
            &[TargetRef::Player(PlayerId(0))],
        ));

        // (b) `All`: the verdict equals `!legal_new_targets.is_empty()`,
        // regardless of any individual position's pool.
        assert!(retarget_prompt_is_dischargeable(
            &RetargetScope::All,
            &[vec![]],
            &[TargetRef::Player(PlayerId(0))],
        ));
        assert!(!retarget_prompt_is_dischargeable(
            &RetargetScope::All,
            &[vec![]],
            &[],
        ));

        // (c) `ForcedTo`: unconditionally `false` — unreachable at the prompt
        // (no construction site in the workspace), and the fail-safe agrees
        // with `apply_retarget`, which rejects it unconditionally.
        assert!(!retarget_prompt_is_dischargeable(
            &RetargetScope::ForcedTo(TargetRef::Player(PlayerId(0))),
            &[vec![TargetRef::Player(PlayerId(0))]],
            &[TargetRef::Player(PlayerId(0))],
        ));
    }

    /// B10's class, structurally: a `Single` prompt whose position 0 has an
    /// empty pool is undischargeable even though the union is not.
    #[test]
    fn retarget_prompt_is_dischargeable_single_position_empty_pool_union_nonempty() {
        assert!(!retarget_prompt_is_dischargeable(
            &RetargetScope::Single,
            &[vec![]],
            &[
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
        ));
    }
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// CR 303.4a + CR 115.7: Retargeting an Aura spell must enumerate every
    /// permanent the Aura could legally enchant (via its `Keyword::Enchant`
    /// filter), not just keep the original host. Regression test for Bolt Bend
    /// vs. an Aura on the stack: before the fix, the Aura's placeholder spell
    /// effect (`Unimplemented`, whose `target_filter()` is `None`) collapsed the
    /// legal set to the current target, so the player could never pick a new host.
    #[test]
    fn retarget_aura_spell_enumerates_other_enchantable_hosts() {
        let mut state = GameState::new_two_player(42);

        // Two enchantable creatures on the battlefield: the current host and an
        // alternative the player should be able to redirect the Aura onto.
        let host_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear A".into(),
            Zone::Battlefield,
        );
        let host_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear B".into(),
            Zone::Battlefield,
        );
        for id in [host_a, host_b] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }

        // An Aura spell on the stack, currently targeting host_a, carrying the
        // placeholder spell ability the cast path synthesizes for Auras (its
        // effect has no target filter; targeting is via the Enchant keyword).
        let aura_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Test Aura".into(),
            Zone::Stack,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types = vec![CoreType::Enchantment];
            aura.card_types.subtypes = vec!["Aura".to_string()];
            aura.keywords = vec![Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            ))];
        }
        let aura_spell_ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(host_a)],
            aura_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(3),
                ability: Some(Box::new(aura_spell_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Bolt Bend: ChangeTargets targeting the Aura spell, no forced target.
        let bolt_bend = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: RetargetScope::Single,
                forced_to: None,
            },
            vec![TargetRef::Object(aura_id)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &bolt_bend, &mut events).unwrap();

        let WaitingFor::RetargetChoice {
            current_targets,
            legal_new_targets,
            ..
        } = &state.waiting_for
        else {
            panic!("expected RetargetChoice, got {:?}", state.waiting_for);
        };
        assert_eq!(current_targets, &vec![TargetRef::Object(host_a)]);
        // Discriminating assertion: the alternative host is offered, so the
        // player can actually redirect the Aura. Pre-fix this list was [host_a].
        assert!(
            legal_new_targets.contains(&TargetRef::Object(host_b)),
            "expected alternative enchantable host in legal targets, got {legal_new_targets:?}"
        );
        assert!(legal_new_targets.contains(&TargetRef::Object(host_a)));
    }

    /// CR 608.2b + CR 115.7 + CR 303.4a: End-to-end regression for Bolt Bend
    /// retargeting an Aura spell, driving the real `stack::resolve_top` pipeline.
    /// Guards two stacked bugs:
    ///   1. `Effect::ChangeTargets` was absent from `Effect::target_filter()`, so
    ///      resolution-time re-validation (CR 608.2b) fell to the battlefield-only
    ///      default and dropped the stack-spell target → Bolt Bend always fizzled
    ///      before its effect ran (no `RetargetChoice`).
    ///   2. Once it stopped fizzling, the Aura's hosts had to be enumerated via
    ///      its `Keyword::Enchant` filter (CR 303.4a), not its placeholder effect.
    ///
    /// Pre-fix, `resolve_top` left `waiting_for == Priority` with Bolt Bend in the
    /// graveyard and the Aura untouched. Post-fix it pauses on `RetargetChoice`
    /// offering every other enchantable creature.
    #[test]
    fn bolt_bend_retargets_aura_spell_via_resolve_top() {
        use crate::types::ability::{FilterProp, TypedFilter};

        let mut state = GameState::new_two_player(42);

        // Current host + an alternative host on the battlefield.
        let host_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear A".into(),
            Zone::Battlefield,
        );
        let host_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear B".into(),
            Zone::Battlefield,
        );
        for id in [host_a, host_b] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }

        // Aura spell on the stack, targeting host_a, with the placeholder spell
        // ability the cast path synthesizes for Auras.
        let aura_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Test Aura".into(),
            Zone::Stack,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types = vec![CoreType::Enchantment];
            aura.card_types.subtypes = vec!["Aura".to_string()];
            aura.keywords = vec![Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            ))];
        }
        let aura_spell_ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(host_a)],
            aura_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(3),
                ability: Some(Box::new(aura_spell_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Bolt Bend on top of the stack, targeting the Aura spell, with its real
        // filter: (StackSpell & HasSingleTarget) | (StackAbility & HasSingleTarget).
        let single = TargetFilter::Typed(TypedFilter {
            type_filters: vec![],
            controller: None,
            properties: vec![FilterProp::HasSingleTarget],
        });
        let bb_filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::And {
                    filters: vec![TargetFilter::StackSpell, single.clone()],
                },
                TargetFilter::And {
                    filters: vec![
                        TargetFilter::StackAbility {
                            controller: None,
                            tag: None,
                            kind: None,
                        },
                        single,
                    ],
                },
            ],
        };
        let bolt_bend = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Bolt Bend".into(),
            Zone::Stack,
        );
        let bb_ability = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: bb_filter,
                scope: RetargetScope::Single,
                forced_to: None,
            },
            vec![TargetRef::Object(aura_id)],
            bolt_bend,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: bolt_bend,
            source_id: bolt_bend,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(4),
                ability: Some(Box::new(bb_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // Bolt Bend must NOT fizzle: it pauses on RetargetChoice (the aura spell
        // stays on the stack awaiting the new host), rather than going to the
        // graveyard with waiting_for == Priority.
        let WaitingFor::RetargetChoice {
            current_targets,
            legal_new_targets,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "expected RetargetChoice (Bolt Bend fizzled instead), got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(current_targets, &vec![TargetRef::Object(host_a)]);
        assert!(
            legal_new_targets.contains(&TargetRef::Object(host_b)),
            "alternative enchantable host must be offered, got {legal_new_targets:?}"
        );
        assert!(state.stack.iter().any(|e| e.id == aura_id));

        // CR 115.7: A single-target retarget resolves through the universal
        // `ChooseTarget` board-click action — the player picks the new host
        // directly on the battlefield rather than through the dialog. The Aura
        // spell's target must update to the chosen host and priority resumes.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(host_b)),
            },
        )
        .expect("board-click retarget should succeed");

        let aura_targets = state
            .stack
            .iter()
            .find(|e| e.id == aura_id)
            .and_then(|e| e.ability())
            .map(|a| a.targets.clone())
            .expect("aura spell still on stack with targets");
        assert_eq!(
            aura_targets,
            vec![TargetRef::Object(host_b)],
            "ChooseTarget board-click must retarget the Aura to the chosen host"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 115.7 + CR 109.4: Retargeting a mass effect that targets a player via
    /// a population filter ("tap all creatures target player controls" —
    /// `SetTapState { scope: All, target: Typed{Creature, controller:
    /// TargetPlayer} }`) must enumerate the *other* player as a legal new
    /// target. Such effects surface a player target slot, but their
    /// `Effect::target_filter()` is `None` (the `target` field is a
    /// resolution-time population scan). Pre-fix, `legal_new_targets` collapsed
    /// to the current target, so Deflecting Swat offered the retarget dialog but
    /// no actual alternative — the player could never redirect the spell.
    /// Regression test for the reported Deflecting Swat bug.
    #[test]
    fn retarget_mass_player_effect_offers_other_player() {
        use crate::types::ability::{ControllerRef, EffectScope, TapStateChange};

        let mut state = GameState::new_two_player(42);

        // "Tap all creatures target player controls" on the stack, cast by
        // PlayerId(1), currently targeting PlayerId(0).
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Sleep-like Spell".into(),
            Zone::Stack,
        );
        let population_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetPlayer));
        let tap_ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: population_filter,
                scope: EffectScope::All,
                state: TapStateChange::Tap,
            },
            vec![TargetRef::Player(PlayerId(0))],
            spell_id,
            PlayerId(1),
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(Box::new(tap_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Deflecting Swat: ChangeTargets (scope All — "choose new targets")
        // targeting the spell, cast by PlayerId(0).
        let deflecting_swat = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: RetargetScope::All,
                forced_to: None,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &deflecting_swat, &mut events).unwrap();

        let WaitingFor::RetargetChoice {
            current_targets,
            legal_new_targets,
            ..
        } = &state.waiting_for
        else {
            panic!("expected RetargetChoice, got {:?}", state.waiting_for);
        };
        assert_eq!(current_targets, &vec![TargetRef::Player(PlayerId(0))]);
        // Discriminating assertion: the OTHER player must be offered so the
        // retarget can actually change the target. Pre-fix this was [Player(0)].
        assert!(
            legal_new_targets.contains(&TargetRef::Player(PlayerId(1))),
            "expected the other player offered as a legal new target, got {legal_new_targets:?}"
        );

        // CR 115.7d: Drive the production retarget action end-to-end — submitting
        // the new player must actually redirect the spell to PlayerId(1), so the
        // "tap all creatures" effect will resolve against the opponent's board.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::RetargetSpell {
                new_targets: vec![TargetRef::Player(PlayerId(1))],
            },
        )
        .expect("retarget submission should succeed");
        let new_targets = state
            .stack
            .iter()
            .find(|e| e.id == spell_id)
            .and_then(|e| e.ability())
            .map(|a| a.targets.clone())
            .expect("spell remains on stack with targets");
        assert_eq!(new_targets, vec![TargetRef::Player(PlayerId(1))]);
    }

    /// CR 115.7: Retarget effects operate on the existing targets of the target
    /// spell or ability. If the chosen stack entry has no targets, Deflecting
    /// Swat resolves as a no-op instead of opening an impossible
    /// `RetargetChoice` with zero slots.
    #[test]
    fn choose_new_targets_on_targetless_spell_resolves_without_choice() {
        let mut state = GameState::new_two_player(42);

        let targetless_spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Targetless Spell".into(),
            Zone::Stack,
        );
        let targetless_ability =
            ResolvedAbility::new(Effect::NoOp, vec![], targetless_spell, PlayerId(1));
        state.stack.push_back(StackEntry {
            id: targetless_spell,
            source_id: targetless_spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(Box::new(targetless_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let deflecting_swat = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: RetargetScope::All,
                forced_to: None,
            },
            vec![TargetRef::Object(targetless_spell)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &deflecting_swat, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::RetargetChoice { .. }),
            "targetless spell must not open RetargetChoice"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::EffectResolved { .. })),
            "targetless retarget should resolve as a no-op"
        );
        let targets = state
            .stack
            .front()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone())
            .expect("targetless spell remains on stack");
        assert!(targets.is_empty());
    }

    /// CR 115.7b: "Change a target ... to this permanent" still has to obey
    /// the targeted spell's own target restriction. Spellskite cannot become
    /// the target of "destroy target nonartifact creature" because it is an
    /// artifact creature.
    #[test]
    fn forced_retarget_ignores_illegal_self_target() {
        let mut state = GameState::new_two_player(42);

        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear).unwrap().card_types.core_types = vec![CoreType::Creature];

        let spellskite = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spellskite".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&spellskite)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact, CoreType::Creature];

        let spell_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Test Doom Blade".into(),
            Zone::Stack,
        );
        let nonartifact_creature = TargetFilter::Typed(
            TypedFilter::creature().with_type(TypeFilter::Non(Box::new(TypeFilter::Artifact))),
        );
        let destroy_ability = ResolvedAbility::new(
            Effect::Destroy {
                target: nonartifact_creature,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear)],
            spell_id,
            PlayerId(1),
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(3),
                ability: Some(Box::new(destroy_ability)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let spellskite_ability = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: RetargetScope::Single,
                forced_to: Some(TargetFilter::SelfRef),
            },
            vec![TargetRef::Object(spell_id)],
            spellskite,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &spellskite_ability, &mut events).unwrap();

        let targets = state
            .stack
            .front()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone())
            .expect("targeted spell remains on stack");
        assert_eq!(targets, vec![TargetRef::Object(bear)]);
    }
}

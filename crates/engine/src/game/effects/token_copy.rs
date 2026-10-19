use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::{DisplaySource, GameObject};
use crate::game::layers::{compute_current_copiable_values, remove_core_type, remove_subtype_set};
use crate::game::quantity::resolve_quantity;
use crate::game::{targeting, zones};
use crate::types::ability::{
    ContinuousModification, CopiableValues, Effect, EffectError, EffectKind, ResolvedAbility,
    StaticDefinition, TargetFilter, TargetRef, TriggerCondition, TriggerDefinition,
};
use crate::types::card::{PrintedCardRef, PrintedLoyalty, TokenArtDescriptor, TokenImageRef};
#[cfg(test)]
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CopyTokenEntryTail, GameState, LiminalEntry, PendingCopyTokenBatch, PendingCopyTokenResolution,
    PendingCounterPostAction, PendingLiminalEntryResume, WaitingFor,
};
use crate::types::game_state::{
    PendingContinuation, TokenCreationBatch, TokenCreationCallerState, TokenCreationPending,
    TokenCreationProgress, TokenCreationWork,
};
use crate::types::identifiers::TokenCreationFrameId;
use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use crate::types::proposed_event::TokenCreationInstructions;
use crate::types::proposed_event::{
    CopyTokenSpec, EtbTapState, ProposedEvent, TokenCharacteristics,
};
use crate::types::resolution::ChildStackDepth;
use crate::types::resolved_commands::{
    ResolvedCopyBodyModifications, ResolvedTokenBody, ResolvedTokenCreationCommand,
};
use crate::types::zones::Zone;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

/// CR 707.2 / CR 707.5: Create a token that's a copy of a permanent.
/// Copies copiable characteristics from the target to a newly created token.
///
/// CR 707.2 + CR 614.1a: When `count` resolves to N > 1 (e.g. Rite of
/// Replication kicked = 5), N independent copy-tokens are created. The
/// per-source count is additionally routed through the `CreateToken`
/// replacement pipeline so token-count-doubling replacements (Doubling Season,
/// Adrix and Nev, Parallel Lives, Anointed Procession, Mondrak) apply uniformly
/// to copy-token creation, exactly as they do to predefined `Effect::Token`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    begin_copy_creation(state, ability, TokenCreationCallerState::Plain, events).map(|_| ())
}

pub(crate) fn begin_copy_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    caller: TokenCreationCallerState,
    events: &mut Vec<GameEvent>,
) -> Result<TokenCreationProgress, EffectError> {
    let work = prepare_copy_creation(state, ability)?;
    Ok(begin_creation(
        state,
        ability,
        work,
        EffectKind::from(&ability.effect),
        caller,
        events,
    ))
}

pub(crate) fn prepare_copy_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
) -> Result<VecDeque<TokenCreationWork>, EffectError> {
    // Extract fields from effect
    let (
        target_filter,
        owner_filter,
        source_filter,
        enters_attacking,
        tapped,
        count_expr,
        extra_keywords,
        additional_modifications,
    ) = match &ability.effect {
        Effect::CopyTokenOf {
            target,
            owner,
            source_filter,
            enters_attacking,
            tapped,
            count,
            extra_keywords,
            additional_modifications,
        } => (
            target,
            owner,
            source_filter,
            *enters_attacking,
            *tapped,
            count.clone(),
            extra_keywords.clone(),
            additional_modifications.clone(),
        ),
        _ => return Err(EffectError::MissingParam("CopyTokenOf".to_string())),
    };
    // CR 107.3a + CR 601.2b: `count` may be the X announced while casting, which resolves from `ability.chosen_x`.
    let count =
        crate::game::quantity::resolve_quantity_with_targets(state, &count_expr, ability).max(0);

    // CR 109.4 + CR 111.2: The token's creator (and therefore controller) is
    // determined by the `owner` filter. Resolved once, before the creation
    // loops, through the same single-authority helper `Effect::Token` uses so
    // "target opponent creates a token that's a copy of it" places the copy
    // under the chosen opponent's control rather than the trigger controller's.
    let Some(token_owner) =
        crate::game::effects::token::resolve_token_owner(state, ability, owner_filter)
    else {
        return Ok(VecDeque::new());
    };

    // Step 1: Resolve the copy source list.
    // CR 608.2c + 603.10a: LTB self-trigger patterns such as Vaultborn Tyrant
    // ("create a token that's a copy of it") and Ochre Jelly's delayed trigger
    // emit `target: ParentTarget` / `SelfRef` with empty `ability.targets`.
    // In a top-level trigger there is no parent chain, so the anaphor refers to
    // the source object itself. `TriggeringSource` is deliberately excluded:
    // it resolves via `state.current_trigger_event`, not `source_id`.
    //
    // CR 115.1d + CR 601.2c: For "any number of target X" / "for each of them,
    // create a token …" (e.g., Twinflame), `ability.targets` carries N >= 1
    // object refs and the resolver creates one copy per target.
    //
    // Zone-eligibility: unlike `Bounce` / `ChangeZone`, a copy source other
    // than the ability's own source is read live through `copy_source`
    // (`compute_current_copiable_values`, which is zone-agnostic) — so such
    // a source in the graveyard is fine.
    //
    // CR 608.2h + CR 707.2: "a copy of this creature" (`SelfRef`) reads its own
    // source through `copy_source`, which supplies the source's last-known
    // copiable values once it has left the zone the ability identified it in.
    let copy_sources: Vec<CopySource> = if copies_own_source(&ability.effect) {
        copy_source(state, ability, CopySourceReferent::OwnSource)
            .into_iter()
            .collect()
    } else {
        let copy_source_ids: Vec<ObjectId> = if let Some(source_filter) = source_filter {
            let zones = {
                let explicit_zones = source_filter.extract_zones();
                if explicit_zones.is_empty() {
                    vec![Zone::Battlefield]
                } else {
                    explicit_zones
                }
            };
            let filter_ctx = FilterContext::from_ability(ability);
            zones
                .into_iter()
                .flat_map(|zone| targeting::zone_object_ids(state, zone))
                .filter(|id| matches_target_filter(state, *id, source_filter, &filter_ctx))
                .collect()
        } else if matches!(target_filter, TargetFilter::CostPaidObject) {
            // CR 400.7: resolve through the shared live-reference guard — a
            // referent that changed zones is a new object and must not become the
            // copy source (no fallback to a same-id object).
            // An ABSENT referent is a malformed ability (nothing was ever bound) and
            // stays a parameter error. A PRESENT but stale referent is different:
            // the object was bound and then became a new object (CR 400.7), so the
            // copy has no legal source and resolves as a clean no-op through the
            // empty-source path below, which still emits `EffectResolved`.
            match ability.cost_paid_object.as_ref() {
                None => {
                    return Err(EffectError::MissingParam(
                        "CopyTokenOf requires a cost-paid object".to_string(),
                    ));
                }
                Some(snapshot) => snapshot
                    .live_object_id(state)
                    .map(|id| vec![id])
                    .unwrap_or_default(),
            }
        } else if matches!(
            target_filter,
            TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
        ) {
            let effective_filter =
                crate::game::targeting::resolve_tracked_set_sentinel(state, target_filter.clone());
            let id = match &effective_filter {
                TargetFilter::TrackedSet { id } | TargetFilter::TrackedSetFiltered { id, .. } => {
                    *id
                }
                _ => unreachable!("tracked-set filter resolved to non-tracked filter"),
            };
            let filter_ctx = FilterContext::from_ability(ability);
            state
                .tracked_object_sets
                .get(&id)
                .into_iter()
                .flatten()
                .copied()
                .filter(|id| matches_target_filter(state, *id, &effective_filter, &filter_ctx))
                .collect()
        } else {
            // CR 608.2c + 603.10a: Delegate to the unified 3-tier dispatch so
            // `None` / `ParentTarget` fall back to source only when
            // `ability.targets` is empty. An own-source `SelfRef` never
            // reaches this branch: it is read through `copy_source` above,
            // which never reads `ability.targets`, so a chained
            // `CopyTokenOf { target: SelfRef }` sub-ability cannot inherit the
            // parent's targets via chain propagation in
            // `effects::mod.rs::resolve_ability_chain` (issue #323 class).
            //
            // CR 109.4 + CR 115.1: `CopyTokenOf` may carry a *player* target in
            // `ability.targets` — the `owner` slot for "target opponent creates a
            // token that's a copy of it" (Wedding Ring). The copy *source* axis is
            // object-only, so a context-ref source (`ParentTarget` / `None`) would
            // otherwise see the owner player as a non-empty `ability.targets` and
            // fail to fall back to the source object. Resolve against an
            // object-only view so the two axes never cross-contaminate.
            let object_only_ability;
            let resolution_ability = if ability
                .targets
                .iter()
                .any(|t| matches!(t, TargetRef::Player(_)))
            {
                let mut narrowed = ability.clone();
                // An occurrence projection: object occurrences keep their pins.
                let objects: Vec<usize> = narrowed
                    .targets
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| matches!(t, TargetRef::Object(_)))
                    .map(|(position, _)| position)
                    .collect();
                narrowed.project_target_occurrences(&objects);
                object_only_ability = narrowed;
                &object_only_ability
            } else {
                ability
            };
            crate::game::effects::resolved_effect_object_ids(
                state,
                resolution_ability,
                target_filter,
            )
        };
        copy_source_ids
            .into_iter()
            .map(|id| {
                copy_source(state, ability, CopySourceReferent::Object(id))
                    .ok_or(EffectError::ObjectNotFound(id))
            })
            .collect::<Result<_, _>>()?
    };

    // CR 609.3 + CR 101.3: "Do as much as possible" — when the copy source
    // resolves empty, `CopyTokenOf` is a clean zero-token no-op rather than an
    // error. This is required for an unattached Springheart Nantuko: its
    // `target: AttachedTo` host resolves empty when the card is not bestowed
    // onto a creature, so the copy makes nothing and the chained
    // `Not(IfYouDo)` Insect-token fallback can still fire. `EffectResolved` is
    // still emitted so the chain treats the effect as resolved.
    if copy_sources.is_empty() {
        // No tokens created — clear the per-resolution token-id ledger so a
        // downstream "the token created this way" anaphor does not pick up a
        // stale id from an earlier resolution. Engine bookkeeping, not a
        // CR-specified rule.
        return Ok(VecDeque::new());
    }

    // CR 707.2 + CR 115.1d: Create `count` independent copy-tokens per copy
    // source. All source values were read in Step 1, before the first creation,
    // so later SBAs (e.g., legendary rule) see identical copies. The drain can
    // pause and resume when the `CreateToken` replacement pipeline requires a
    // CR 616.1 choice.
    let mut remaining = VecDeque::with_capacity(copy_sources.len());
    for source in copy_sources {
        remaining.push_back(PendingCopyTokenBatch {
            owner: token_owner,
            count: count as u32,
            copy: Box::new(CopyTokenSpec {
                values: Box::new(source.values),
                display_source: source.display_source,
                printed_ref: source.printed_ref,
                token_image_ref: source.token_image_ref,
                // Created copy-tokens derive their descriptor from their
                // own base at injection (copy exceptions included), so the
                // source's captured body is deliberately not carried here.
                token_art: None,
                extra_keywords: extra_keywords.clone(),
                additional_modifications: additional_modifications.clone(),
                tapped,
                enters_attacking,
                sacrifice_at: ability.duration.clone(),
                source_id: ability.source_id,
                controller: ability.controller,
            }),
        });
    }

    let mut work = copy_batches_to_work(state, remaining);
    if ability.context.token_creation_parent.is_some() {
        for item in &mut work {
            let batch = match item {
                TokenCreationWork::Proposed(batch) | TokenCreationWork::Approved(batch) => batch,
            };
            batch.applied.clone_from(&ability.replacement_applied);
        }
    }
    Ok(work)
}

/// CR 707.2 + CR 608.2h: which copy source a copy effect reads.
pub(crate) enum CopySourceReferent {
    /// The ability's own source ("a copy of this creature"), bound to the
    /// incarnation the ability captured.
    OwnSource,
    /// An object the effect's route identified at resolution.
    Object(ObjectId),
}

/// CR 707.2: one copy source as a copy effect reads it: the incarnation it
/// was read from, its copiable values, and the display identity the copy
/// records with them (not copiable).
pub(crate) struct CopySource {
    pub(crate) incarnation: ObjectIncarnationRef,
    pub(crate) values: CopiableValues,
    pub(crate) display_source: DisplaySource,
    pub(crate) printed_ref: Option<PrintedCardRef>,
    pub(crate) token_image_ref: Option<TokenImageRef>,
    pub(crate) token_art: Option<TokenArtDescriptor>,
}

impl CopySource {
    /// CR 707.2: the current copiable values of an object that is still the
    /// copy source.
    fn live(state: &GameState, id: ObjectId) -> Option<Self> {
        let values = compute_current_copiable_values(state, id)?;
        let source = state.objects.get(&id)?;
        Some(Self {
            incarnation: ObjectIncarnationRef::from_object(source),
            values,
            display_source: source.display_source,
            printed_ref: source.printed_ref.clone(),
            token_image_ref: source.token_image_ref.clone(),
            token_art: source.token_art.clone(),
        })
    }

    /// CR 608.2h + CR 707.2: the last-known copiable values of the exact
    /// incarnation `incarnation`, recorded as it left a public zone. The
    /// display routing is derived as the departed-source reader in
    /// `replacement::create_entry_copy_spec_for_replacement` derives it.
    fn last_known(state: &GameState, incarnation: ObjectIncarnationRef) -> Option<Self> {
        let values = state
            .lki_copiable_values_by_incarnation
            .get(&incarnation.object_id)?
            .get(&incarnation.incarnation)?
            .clone();
        let token_image_ref = state
            .lki_by_incarnation
            .get(&incarnation.object_id)
            .and_then(|history| history.get(&incarnation.incarnation))
            .and_then(|lki| lki.token_image_ref.clone());
        Some(Self {
            incarnation,
            values,
            display_source: if token_image_ref.is_some() {
                DisplaySource::Token
            } else {
                DisplaySource::Card
            },
            printed_ref: None,
            token_image_ref,
            token_art: None,
        })
    }
}

/// CR 608.2h + CR 707.2 + CR 400.7: the single authority for what a copy
/// effect copies from a specific copy source, shared by `CopyTokenOf` and
/// `BecomeCopy`. The ability's own source is copied with its current
/// copiable values while it is still the incarnation the ability captured;
/// once that object has left the public zone it was identified in, with its
/// last-known copiable values for that exact incarnation. A later object
/// under the same storage id is a new object and is never the source.
/// `None` when the source supplies neither. The stack's bulk copy admission
/// (`token::try_resolve_copy_batch`) reads it too, on each member's swapped
/// ability, so admission and handler read one copy source per member.
pub(crate) fn copy_source(
    state: &GameState,
    ability: &ResolvedAbility,
    referent: CopySourceReferent,
) -> Option<CopySource> {
    match referent {
        CopySourceReferent::Object(id) => CopySource::live(state, id),
        CopySourceReferent::OwnSource if ability.source_is_current(state) => {
            CopySource::live(state, ability.source_id)
        }
        CopySourceReferent::OwnSource => {
            CopySource::last_known(state, captured_own_source(ability)?)
        }
    }
}

/// CR 707.2: whether `effect` copies its ability's own source: "a copy of
/// this creature" as a token (`CopyTokenOf`) or as a permanent becoming a copy
/// (`BecomeCopy`), and the keyword handlers that build such a copy (Myriad,
/// CR 702.116a; Encore, CR 702.141a).
pub(crate) fn copies_own_source(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            source_filter: None,
            ..
        } | Effect::BecomeCopy {
            target: TargetFilter::SelfRef,
            ..
        } | Effect::Myriad
            | Effect::Encore
    )
}

/// CR 400.7 + CR 608.2h: the `CopyTokenOf` ability a keyword handler builds to
/// copy its own source (Myriad, CR 702.116a; Encore, CR 702.141a). It carries
/// the outer ability's captured source identity, so `copy_source` binds the
/// incarnation the outer ability captured.
pub(crate) fn own_source_copy_ability(effect: Effect, outer: &ResolvedAbility) -> ResolvedAbility {
    let mut ability = ResolvedAbility::new(effect, vec![], outer.source_id, outer.controller);
    ability.trigger_source = outer.trigger_source.clone();
    ability.source_incarnation = outer.source_incarnation;
    ability
}

/// CR 400.7 + CR 400.7j: the source incarnation an ability captured: a
/// trigger's source identity, or the incarnation an activated ability's
/// source had as the ability was put on the stack (for Embalm, Eternalize and
/// Encore, the exile incarnation its own cost created).
fn captured_own_source(ability: &ResolvedAbility) -> Option<ObjectIncarnationRef> {
    match &ability.trigger_source {
        Some(source) => Some(source.identity.reference),
        None => ability
            .source_incarnation
            .map(|incarnation| ObjectIncarnationRef::of(ability.source_id, incarnation)),
    }
}

/// CR 104.4b + CR 608.2h: collect the source incarnations whose last-known
/// copiable values `ability`, or a branch of it, can still read as its own
/// copy source. `GameState::normalize_for_loop` reads them before it erases
/// the abilities' identities.
pub(crate) fn collect_own_copy_sources(
    ability: &ResolvedAbility,
    sources: &mut HashSet<ObjectIncarnationRef>,
) {
    if copies_own_source(&ability.effect) {
        sources.extend(captured_own_source(ability));
    }
    for branch in [&ability.sub_ability, &ability.else_ability]
        .into_iter()
        .flatten()
    {
        collect_own_copy_sources(branch, sources);
    }
}

/// CR 707.2 + CR 614.1a: Route a queue of `PendingCopyTokenBatch`es through the
/// `CreateToken` replacement pipeline and apply path. Single authority shared by
/// `CopyTokenOf` (battlefield-sourced copies) and `CreateTokenCopyFromPool`
/// (format-pool-sourced copies) so the replacement + apply path is never
/// duplicated. The drain can pause and resume when a CR 616.1 replacement choice
/// is required.
pub(crate) fn drive_copy_token_batches(
    state: &mut GameState,
    remaining: VecDeque<PendingCopyTokenBatch>,
    effect_kind: EffectKind,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> TokenCreationProgress {
    let ability = ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
        vec![],
        source_id,
        state.active_player,
    );
    let work = copy_batches_to_work(state, remaining);
    begin_creation(
        state,
        &ability,
        work,
        effect_kind,
        TokenCreationCallerState::Plain,
        events,
    )
}

pub(crate) fn copy_batches_to_work(
    state: &GameState,
    remaining: VecDeque<PendingCopyTokenBatch>,
) -> VecDeque<TokenCreationWork> {
    remaining
        .into_iter()
        .map(|batch| {
            let spec = super::token::copy_probe_spec_for(&batch.copy, &state.all_creature_types);
            let instructions = TokenCreationInstructions {
                tapped: EtbTapState::from_seeded_tapped(batch.copy.tapped),
                enters_attacking: batch.copy.enters_attacking,
            };
            TokenCreationWork::Proposed(TokenCreationBatch {
                owner: batch.owner,
                spec: Box::new(spec),
                copy: Some(batch.copy),
                instructions,
                enter_tapped: instructions.tapped,
                count: batch.count,
                applied: HashSet::new(),
            })
        })
        .collect()
}

/// CR 608.2c: install the instruction owner before invoking any replacement or
/// entry callback. The continuation belongs to this owner through every pause.
pub(crate) fn begin_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    work: VecDeque<TokenCreationWork>,
    effect_kind: EffectKind,
    caller: TokenCreationCallerState,
    events: &mut Vec<GameEvent>,
) -> TokenCreationProgress {
    let id = install_creation(state, ability, work, effect_kind, caller);
    if state
        .resolution_stack
        .token_creation(id)
        .expect("installed")
        .pending
        == Some(TokenCreationPending::CallerSelection)
    {
        return TokenCreationProgress::Pending(id);
    }
    drive_creation(state, id, events)
}

pub(crate) fn install_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    mut work: VecDeque<TokenCreationWork>,
    effect_kind: EffectKind,
    caller: TokenCreationCallerState,
) -> TokenCreationFrameId {
    let mut creator = ability.clone();
    let continuation = creator.sub_ability.take().map(|mut tail| {
        if super::should_propagate_parent_targets(ability, &tail) {
            tail.replace_target_occurrences(super::inherited_parent_occurrences(ability, &tail));
        }
        super::apply_parent_chain_context(&mut tail, ability, None, state);
        Box::new(PendingContinuation::new(tail, state))
    });
    let parent = creator.context.token_creation_parent.take();
    if let Some(parent) = parent {
        assert!(
            state
                .resolution_stack
                .token_creation(parent.frame)
                .is_some(),
            "substitution parent must be resident"
        );
        for item in &mut work {
            let batch = match item {
                TokenCreationWork::Proposed(batch) | TokenCreationWork::Approved(batch) => batch,
            };
            batch.instructions = parent.instructions;
            batch.enter_tapped = parent.enter_tapped;
            batch.spec.tapped = parent.instructions.tapped.resolve(false);
            batch.spec.enters_attacking = parent.instructions.enters_attacking;
            if let Some(copy) = batch.copy.as_mut() {
                copy.tapped = batch.spec.tapped;
                copy.enters_attacking = batch.spec.enters_attacking;
            }
        }
    }
    let mut pending =
        PendingCopyTokenResolution::legacy(VecDeque::new(), effect_kind, ability.source_id);
    pending.result_parent = parent.map(|context| context.frame);
    pending.work = work;
    pending.creator = Some(Box::new(creator));
    pending.continuation = continuation;
    pending.caller = caller;
    let selection = match &pending.caller {
        TokenCreationCallerState::Populate { eligible } if eligible.len() > 1 => {
            pending.pending = Some(TokenCreationPending::CallerSelection);
            Some(eligible.clone())
        }
        _ => None,
    };
    let id = state.resolution_stack.install_token_creation(pending);
    if let Some(valid_tokens) = selection {
        state.waiting_for = WaitingFor::PopulateChoice {
            player: ability.controller,
            source_id: ability.source_id,
            valid_tokens,
        };
    }
    id
}

pub(crate) fn enqueue_creation_expansion(
    state: &mut GameState,
    id: TokenCreationFrameId,
    work: TokenCreationBatch,
) {
    state
        .resolution_stack
        .token_creation_mut(id)
        .expect("expansion must name its resident instruction")
        .work
        .push_front(TokenCreationWork::Proposed(work));
}

/// Store a settled primary behind the expansion work queued by its replacements.
pub(crate) fn approve_creation_batch(
    state: &mut GameState,
    event: ProposedEvent,
) -> TokenCreationFrameId {
    let ProposedEvent::CreateToken {
        creation_frame: Some(id),
        instructions: Some(instructions),
        owner,
        spec,
        copy,
        enter_tapped,
        count,
        applied,
    } = event
    else {
        panic!("current approved creation must retain exact instruction authority")
    };
    let substitution = state
        .active_post_replacement_drains()
        .and_then(|drains| drains.resident())
        .is_some_and(|drain| {
            drain
                .token_creation_parent
                .is_some_and(|parent| parent.frame == id)
        });
    let pending = state
        .resolution_stack
        .token_creation_mut(id)
        .expect("approved creation owner must be resident");
    let previous_work = pending
        .replacement_queue_tail
        .take()
        .expect("replacement owns its queued-tail boundary");
    let expansions = pending
        .work
        .len()
        .checked_sub(previous_work)
        .expect("replacement cannot consume queued work");
    pending.work.insert(
        expansions,
        TokenCreationWork::Approved(TokenCreationBatch {
            owner,
            spec,
            copy,
            instructions,
            enter_tapped,
            count,
            applied,
        }),
    );
    pending.pending = substitution.then_some(TokenCreationPending::Substitution);
    id
}

/// CR 111.1 + CR 608.2c: drive one resident instruction's ordered work. Entry
/// admission alone supplies its result; mirrors never establish membership.
pub(crate) fn drive_creation(
    state: &mut GameState,
    id: TokenCreationFrameId,
    events: &mut Vec<GameEvent>,
) -> TokenCreationProgress {
    loop {
        assert!(
            state.resolution_stack.token_creation(id).is_some(),
            "creation identity must be live"
        );
        if state
            .active_copy_token()
            .and_then(|frame| frame.creation_id)
            != Some(id)
        {
            return TokenCreationProgress::Pending(id);
        }
        let item = {
            let pending = state
                .resolution_stack
                .token_creation_mut(id)
                .expect("checked creation owner");
            if matches!(
                pending.pending,
                Some(TokenCreationPending::Replacement | TokenCreationPending::CallerSelection)
            ) {
                return TokenCreationProgress::Pending(id);
            }
            pending.pending = None;
            pending.work.pop_front()
        };
        let Some(item) = item else {
            if complete_creation_segment(state, id, events) {
                continue;
            }
            if state
                .active_copy_token()
                .and_then(|pending| pending.creation_id)
                != Some(id)
                || !matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                return TokenCreationProgress::Pending(id);
            }
            return finish_creation(state, id, events);
        };
        let (batch, proposed) = match item {
            TokenCreationWork::Proposed(batch) => (batch, true),
            TokenCreationWork::Approved(batch) => (batch, false),
        };
        if batch.count == 0 {
            continue;
        }
        let event = ProposedEvent::CreateToken {
            creation_frame: Some(id),
            instructions: Some(batch.instructions),
            owner: batch.owner,
            spec: batch.spec,
            copy: batch.copy,
            enter_tapped: batch.enter_tapped,
            count: batch.count,
            applied: batch.applied,
        };
        if proposed {
            let previous_work = state
                .resolution_stack
                .token_creation(id)
                .expect("resident")
                .work
                .len();
            let pending = state
                .resolution_stack
                .token_creation_mut(id)
                .expect("resident");
            pending.pending = Some(TokenCreationPending::Replacement);
            pending.replacement_queue_tail = Some(previous_work);
            match crate::game::replacement::replace_event(state, event, events) {
                crate::game::replacement::ReplacementResult::Execute(event) => {
                    approve_creation_batch(state, event);
                }
                crate::game::replacement::ReplacementResult::Prevented => {
                    let pending = state
                        .resolution_stack
                        .token_creation_mut(id)
                        .expect("resident");
                    pending.pending = None;
                    pending.replacement_queue_tail = None;
                }
                crate::game::replacement::ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return TokenCreationProgress::Pending(id);
                }
            }
        } else {
            state
                .resolution_stack
                .token_creation_mut(id)
                .expect("resident")
                .pending = Some(TokenCreationPending::Entry);
            if !super::token::apply_create_token_after_replacement_with_created_ids(
                state,
                event,
                Vec::new(),
                crate::types::game_state::PendingEffectResolutionEvent::Suppress,
                events,
            ) {
                return TokenCreationProgress::Pending(id);
            }
            state
                .resolution_stack
                .token_creation_mut(id)
                .expect("resident")
                .pending = None;
        }
    }
}

fn complete_creation_segment(
    state: &mut GameState,
    id: TokenCreationFrameId,
    events: &mut Vec<GameEvent>,
) -> bool {
    let (caller, creator, members, complete) = {
        let pending = state
            .resolution_stack
            .token_creation_mut(id)
            .expect("resident segment owner");
        let caller = std::mem::take(&mut pending.caller);
        let members = pending.created_incarnations[pending.segment_start..].to_vec();
        pending.segment_start = pending.created_incarnations.len();
        pending.pending = Some(TokenCreationPending::Caller);
        (
            caller,
            pending.creator.as_deref().expect("current creator").clone(),
            members,
            pending.created_incarnations.clone(),
        )
    };
    let result = crate::types::ability::ForwardedResultContext {
        targets: complete
            .iter()
            .map(|member| TargetRef::Object(member.object_id))
            .collect(),
        object_incarnations: complete,
    };
    state.last_created_token_ids = result
        .object_incarnations
        .iter()
        .map(|member| member.object_id)
        .collect();
    let next = match caller {
        TokenCreationCallerState::Plain => None,
        TokenCreationCallerState::EachPlayerCopyChosen => {
            state
                .resolution_stack
                .token_creation_mut(id)
                .expect("resident")
                .caller = TokenCreationCallerState::EachPlayerCopyChosen;
            None
        }
        TokenCreationCallerState::Blocking {
            current,
            mut remaining,
            owner,
        } => {
            super::copy_token_blocking::complete_creation_segment(state, current, &members);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CopyTokenOf,
                source_id: creator.source_id,
                subject: None,
            });
            remaining.pop_front().map(|current| {
                let work = super::copy_token_blocking::copy_work(state, &creator, current, &owner)
                    .expect("bound blocking copy work");
                (
                    TokenCreationCallerState::Blocking {
                        current,
                        remaining,
                        owner,
                    },
                    work,
                )
            })
        }
        TokenCreationCallerState::Encore {
            current,
            mut remaining,
        } => {
            super::encore::complete_creation_segment(state, &creator, current, &members);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CopyTokenOf,
                source_id: creator.source_id,
                subject: None,
            });
            remaining.pop_front().map(|current| {
                (
                    TokenCreationCallerState::Encore { current, remaining },
                    super::encore::copy_work(state, &creator).expect("bound Encore source"),
                )
            })
        }
        TokenCreationCallerState::Myriad {
            current,
            mut remaining,
            defender,
        } => {
            super::myriad::complete_creation_segment(state, current, &members);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CopyTokenOf,
                source_id: creator.source_id,
                subject: None,
            });
            remaining.pop_front().map(|current| {
                (
                    TokenCreationCallerState::Myriad {
                        current,
                        remaining,
                        defender,
                    },
                    super::myriad::copy_work(state, &creator).expect("bound Myriad source"),
                )
            })
        }
        TokenCreationCallerState::Populate { eligible } => {
            if !eligible.is_empty() {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::CopyTokenOf,
                    source_id: creator.source_id,
                    subject: None,
                });
            }
            None
        }
        TokenCreationCallerState::Investigate { player } => {
            super::investigate::complete_creation(state, player, events);
            None
        }
        TokenCreationCallerState::Amass {
            controller,
            subtype,
            count,
        } => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Token,
                source_id: creator.source_id,
                subject: None,
            });
            super::amass::continue_amass_after_token_creation(
                state,
                controller,
                &subtype,
                count,
                &creator,
                Some(id),
                events,
            );
            None
        }
        TokenCreationCallerState::EmpowerJace { controller, count } => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Token,
                source_id: creator.source_id,
                subject: None,
            });
            super::empower_jace::continue_after_creation(
                state,
                controller,
                creator.source_id,
                count,
                events,
            );
            None
        }
        TokenCreationCallerState::Debug {
            run_etb,
            preset,
            body,
        } => {
            crate::game::engine_debug::complete_token_creation(
                state,
                &result,
                preset,
                body.as_deref(),
                run_etb,
                events,
            );
            None
        }
    };
    let pending = state
        .resolution_stack
        .token_creation_mut(id)
        .expect("caller step cannot retire its owner");
    if let Some((caller, work)) = next {
        pending.caller = caller;
        pending.work.extend(work);
        pending.pending = None;
        true
    } else {
        false
    }
}

fn finish_creation(
    state: &mut GameState,
    id: TokenCreationFrameId,
    events: &mut Vec<GameEvent>,
) -> TokenCreationProgress {
    let pending = state
        .take_active_copy_token()
        .expect("creation retires only at top")
        .expect("resident creation");
    assert_eq!(pending.creation_id, Some(id));
    let result = crate::types::ability::ForwardedResultContext {
        targets: pending
            .created_incarnations
            .iter()
            .map(|member| TargetRef::Object(member.object_id))
            .collect(),
        object_incarnations: pending.created_incarnations,
    };
    if let Some(parent) = pending.result_parent {
        for member in &result.object_incarnations {
            state
                .resolution_stack
                .admit_token_creation(parent, *member)
                .expect("substitution result merges once into its exact live parent");
        }
    }
    state.last_created_token_ids = result
        .object_incarnations
        .iter()
        .map(|member| member.object_id)
        .collect();
    if matches!(
        pending.caller,
        TokenCreationCallerState::EachPlayerCopyChosen
    ) {
        let parent = state
            .active_each_player_copy_chosen_mut()
            .expect("creation child has its adjacent EachPlayer owner");
        assert_eq!(
            parent.copy_status,
            Some(crate::types::game_state::TokenCreationCopyStatus::Pending(
                id
            ))
        );
        parent.copy_status =
            Some(crate::types::game_state::TokenCreationCopyStatus::Completed(result.clone()));
    }
    if let Some(creator) = pending.creator.as_deref() {
        match pending.effect_kind {
            EffectKind::Encore => super::encore::complete_creation(state, creator, &result, events),
            EffectKind::Myriad => super::myriad::complete_creation(state, creator, &result, events),
            _ => {}
        }
    }
    if !matches!(
        pending.effect_kind,
        EffectKind::Amass | EffectKind::EmpowerJace
    ) {
        events.push(GameEvent::EffectResolved {
            kind: pending.effect_kind,
            source_id: pending.source_id,
            subject: None,
        });
    }
    if let Some(mut continuation) = pending.continuation {
        continuation.chain.context.token_creation_parent = None;
        continuation.chain.context.forwarded_result_context = Some(Box::new(result.clone()));
        super::prepare_chain_child(state, Some(&continuation.chain), events);
        state.park_ability_continuation(*continuation);
        super::drain_pending_continuation(state, events);
    }
    TokenCreationProgress::Completed(result)
}

/// The historical id-less choice has no recoverable creation owner. Keep its
/// old applied/count authority isolated from every current creation constructor.
pub(crate) fn resolve_legacy_copy_token_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let work = prepare_copy_creation(state, ability)?;
    let remaining = work
        .into_iter()
        .map(|work| {
            let TokenCreationWork::Proposed(batch) = work else {
                unreachable!("new legacy copy proposal")
            };
            PendingCopyTokenBatch {
                owner: batch.owner,
                count: batch.count,
                copy: batch.copy.expect("copy-only legacy selected tail"),
            }
        })
        .collect();
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    drain_copy_token_resolution(
        state,
        PendingCopyTokenResolution::legacy(remaining, EffectKind::CopyTokenOf, ability.source_id),
        events,
    );
    Ok(())
}

pub(crate) fn drain_pending_copy_token_resolution(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if let Some(id) = state
        .active_copy_token()
        .and_then(|pending| pending.creation_id)
    {
        let _ = drive_creation(state, id, events);
        return;
    }
    let Some(pending) = state
        .take_active_copy_token()
        .expect("copy-token drain may consume only the active CopyToken frame")
    else {
        return;
    };
    drain_copy_token_resolution(state, pending, events);
    // CR 608.2c + CR 117.3b: a continuation left directly beneath the consumed
    // owner resumes now, before any player receives priority.
    if matches!(
        state.waiting_for,
        crate::types::game_state::WaitingFor::Priority { .. }
    ) && state.active_ability_continuation().is_some()
    {
        super::drain_pending_continuation(state, events);
    }
}

fn drain_copy_token_resolution(
    state: &mut GameState,
    mut pending: PendingCopyTokenResolution,
    events: &mut Vec<GameEvent>,
) {
    while let Some(batch) = pending.remaining.pop_front() {
        if batch.count == 0 {
            continue;
        }
        let stack_depth_before_batch = state.resolution_stack.capture_child_boundary();
        let spec = super::token::copy_probe_spec_for(&batch.copy, &state.all_creature_types);
        let mut spec = spec;
        spec.tapped = batch.copy.tapped;
        spec.enters_attacking = batch.copy.enters_attacking;
        let enter_tapped = EtbTapState::from_seeded_tapped(batch.copy.tapped);
        let proposed = ProposedEvent::CreateToken {
            creation_frame: None,
            instructions: None,
            owner: batch.owner,
            spec: Box::new(spec),
            copy: Some(batch.copy),
            enter_tapped,
            count: batch.count,
            // CR 614.6 + CR 616.1: a copy-token-substitution continuation
            // (Moonlit Meditation) inherits the originating event's applied set
            // so the substitution replacement cannot re-prompt on its own copy
            // tokens. `None` for normal copy effects (Springheart, Twinflame,
            // populate) → empty set → byte-identical to the prior
            // `HashSet::new()`. A *different* source's replacement (Doubling
            // Season's rid) is absent from the seed and still applies (#1511).
            applied: state
                .post_replacement_token_choice_applied
                .clone()
                .unwrap_or_default(),
        };

        match crate::game::replacement::replace_event(state, proposed, events) {
            crate::game::replacement::ReplacementResult::Execute(event) => {
                if !super::token::apply_create_token_after_replacement(state, event, events) {
                    pending
                        .created_ids
                        .extend(state.last_created_token_ids.clone());
                    park_copy_token_after_current_batch(state, pending, stack_depth_before_batch);
                    return;
                }
                pending
                    .created_ids
                    .extend(state.last_created_token_ids.clone());
            }
            crate::game::replacement::ReplacementResult::Prevented => {}
            crate::game::replacement::ReplacementResult::NeedsChoice(player) => {
                park_copy_token_after_current_batch(state, pending, stack_depth_before_batch);
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return;
            }
        }
    }

    // CR 603.7 + CR 701.36a: Record created token IDs so sub-abilities can
    // reference them via `TargetFilter::LastCreated` ("the token created this
    // way", "it") and so "those tokens" plural anaphor in delayed triggers
    // captures the full list. Mirrors `token::apply_create_token`.
    state.last_created_token_ids = pending.created_ids;

    events.push(GameEvent::EffectResolved {
        kind: pending.effect_kind,
        source_id: pending.source_id,
        subject: None,
    });
}

/// Park a copy-token owner as the direct paused frame, or below the complete
/// child stack its batch raised. The captured boundary preserves the actual
/// parent/child relationship without searching for a buried CopyToken frame.
fn park_copy_token_after_current_batch(
    state: &mut GameState,
    pending: PendingCopyTokenResolution,
    stack_depth_before_batch: ChildStackDepth,
) {
    match state
        .resolution_stack
        .capture_child_boundary()
        .cmp(&stack_depth_before_batch)
    {
        std::cmp::Ordering::Less => {
            panic!("copy-token batch removed a parent frame before it could be re-parked")
        }
        std::cmp::Ordering::Equal => state.push_copy_token(pending),
        std::cmp::Ordering::Greater => state
            .insert_copy_token_parent_at_child_boundary(pending, stack_depth_before_batch)
            .expect("copy-token parent must be inserted below its complete child stack"),
    }
}

/// CR 601.2f + CR 603.4: Triggers gated on a cast-time additional-cost payment
/// ("if its offspring cost was paid", "if it was kicked") are inert on a token
/// copy — the token was created, not cast, so the payment condition can never
/// hold. Only the payment-gated condition is stripped: persistent battlefield
/// abilities that *observe* spell casts (Magecraft's `SpellCastOrCopy`,
/// "whenever you cast a [type] spell" `SpellCast`) are copiable text per
/// CR 707.2 and must survive on the copy.
fn is_cast_payment_gated_trigger(trig: &TriggerDefinition) -> bool {
    matches!(
        trig.condition,
        Some(TriggerCondition::AdditionalCostPaid { .. })
    )
}

/// CR 601.2f + CR 707.2: Remove spell-casting-only copiable characteristics
/// from a freshly created copy token (Offspring/Kicker keywords, "if it was
/// kicked" / "if offspring was paid" ETB triggers, etc.).
fn strip_spell_casting_copiable_characteristics(obj: &mut GameObject) {
    obj.keywords.retain(|kw| !kw.is_spell_casting_only());
    obj.base_keywords.retain(|kw| !kw.is_spell_casting_only());
    obj.trigger_definitions
        .retain(|entry| !is_cast_payment_gated_trigger(entry.definition()));
    Arc::make_mut(&mut obj.base_trigger_definitions)
        .retain(|trig| !is_cast_payment_gated_trigger(trig));
}

/// CR 111.10 + CR 702.175a: When a card-related token preset exists (Offspring,
/// set-specific copy tokens), route the copy through the token art database
/// instead of rendering as a card-copy with a "Copy" badge.
fn resolve_predefined_token_display(
    state: &mut GameState,
    copy_source_id: ObjectId,
    token_id: ObjectId,
) {
    let body = {
        let token = match state.objects.get(&token_id) {
            Some(token) if token.display_source == DisplaySource::Card => token,
            _ => return,
        };
        TokenCharacteristics {
            display_name: token.name.clone(),
            power: token.power,
            toughness: token.toughness,
            loyalty: None,
            core_types: token.card_types.core_types.clone(),
            subtypes: token.card_types.subtypes.clone(),
            supertypes: token.card_types.supertypes.clone(),
            colors: token.color.clone(),
            keywords: token.keywords.clone(),
        }
    };
    let Some(image_ref) =
        crate::game::token_presets::find_card_linked_copy_token_ref(state, copy_source_id, &body)
    else {
        return;
    };
    if let Some(token) = state.objects.get_mut(&token_id) {
        token.display_source = DisplaySource::Token;
        token.token_image_ref = Some(image_ref);
    }
}

/// CR 707.2: Finalize a copy token after P/T exceptions and cast-only stripping.
pub(crate) fn finalize_copied_token(
    state: &mut GameState,
    copy_source_id: ObjectId,
    token_id: ObjectId,
) {
    if let Some(token) = state.objects.get_mut(&token_id) {
        strip_spell_casting_copiable_characteristics(token);
    }
    resolve_predefined_token_display(state, copy_source_id, token_id);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_copy_token_after_replacement(
    state: &mut GameState,
    creation_frame: Option<TokenCreationFrameId>,
    token_owner: crate::types::player::PlayerId,
    copy: CopyTokenSpec,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(crate::types::counter::CounterType, u32)>,
    final_count: u32,
    events: &mut Vec<GameEvent>,
) -> CopyTokenApplyStatus {
    apply_copy_token_after_replacement_with_created_ids(
        state,
        creation_frame,
        token_owner,
        copy,
        enter_tapped,
        enter_with_counters,
        final_count,
        Vec::new(),
        events,
    )
}

/// CR 306.5b + CR 707.2 + CR 707.9b: copy exceptions can replace starting
/// loyalty; intrinsic self-entry counters and explicit entry counters all
/// belong to the materializer's actual counter work.
pub(crate) fn copy_token_entry_counters(
    copy: &CopyTokenSpec,
    explicit: &[(crate::types::counter::CounterType, u32)],
) -> Vec<(crate::types::counter::CounterType, u32)> {
    let loyalty =
        copy_starting_loyalty_override(&copy.additional_modifications).or(copy.values.loyalty);
    crate::game::printed_cards::intrinsic_face_counters(loyalty, None)
        .into_iter()
        .chain(crate::game::printed_cards::self_etb_counter_replacements(
            &copy.values.replacement_definitions,
        ))
        .chain(explicit.iter().cloned())
        .collect()
}

/// CR 614.12: the current copy proposal has a later exact TokenEntry only
/// when its immediate exceptions and its complete counter work permit it.
pub(crate) fn copy_token_uses_liminal_entry(
    copy: &CopyTokenSpec,
    explicit: &[(crate::types::counter::CounterType, u32)],
) -> bool {
    copy_token_modifications_are_liminal_immediate(&copy.additional_modifications)
        && copy_token_entry_counters(copy, explicit).is_empty()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_copy_token_after_replacement_with_created_ids(
    state: &mut GameState,
    creation_frame: Option<TokenCreationFrameId>,
    token_owner: crate::types::player::PlayerId,
    copy: CopyTokenSpec,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(crate::types::counter::CounterType, u32)>,
    final_count: u32,
    initial_created_ids: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> CopyTokenApplyStatus {
    let etb_counters = copy_token_entry_counters(&copy, &enter_with_counters);
    let liminal_immediate = copy_token_uses_liminal_entry(&copy, &enter_with_counters);
    let CopyTokenSpec {
        mut values,
        display_source,
        printed_ref,
        token_image_ref,
        token_art,
        extra_keywords,
        additional_modifications,
        tapped,
        enters_attacking,
        sacrifice_at,
        source_id,
        controller,
    } = copy;
    let name = values.name.clone();
    let mut created_ids = initial_created_ids;
    created_ids.reserve(final_count as usize);
    if let Some(loyalty) = copy_starting_loyalty_override(&additional_modifications) {
        values.loyalty = Some(loyalty);
        values.printed_loyalty = Some(PrintedLoyalty::Fixed(loyalty));
    }

    // CR 306.5b + CR 707.2 + CR 707.9b: A token that's a copy of a planeswalker
    // enters with loyalty counters equal to the copied loyalty, except a copy
    // exception may set that starting loyalty to a different value. CR 306.5c
    // makes the counter map the single source of truth for loyalty. The copy
    // path builds the object directly (not through the ZoneChange ETB-counter
    // seeding used by cast/play/effect entries), so seed the intrinsic loyalty
    // counters here, ahead of any explicit `enter_with_counters`, routing them
    // through `add_counter_with_replacement` below so Doubling Season etc. apply
    // (CR 614.1a). Without this a copied planeswalker enters with 0 loyalty
    // counters and dies immediately to CR 704.5i. Copies don't track battle
    // defense (`CopiableValues` has no defense field), so only loyalty is seeded.
    // CR 306.5b loyalty + CR 614.1c "~ enters with N counters" self-replacement
    // (Atraxa's Skitterfang's three oil counters, Hangarback/Walking Ballista
    // +1/+1, etc.) + any explicit counters the creating effect added. The copy
    // path bypasses the ZoneChange replacement pass, so the copied card's
    // intrinsic "enters with counters" replacement is seeded here from its
    // copiable replacement set rather than firing during entry.
    // The shared admission predicate above uses this same exact counter list.
    // CR 205.3m: the live creature-type list the CR 707.9 subtype exceptions
    // resolve against. Loop-invariant, and cloned once so the per-token CR
    // 303.4f/g projection below can be built while `state` is borrowed.
    let all_creature_types = state.all_creature_types.clone();

    for index in 0..final_count {
        if liminal_immediate {
            let (token_id, mut token) =
                super::token::reserve_liminal_token_object(state, token_owner, name.clone());
            let entry_timestamp = state.next_timestamp();

            let copy_spec = Box::new(CopyTokenSpec {
                values: values.clone(),
                display_source,
                printed_ref: printed_ref.clone(),
                token_image_ref: token_image_ref.clone(),
                token_art: token_art.clone(),
                extra_keywords: extra_keywords.clone(),
                additional_modifications: additional_modifications.clone(),
                tapped,
                enters_attacking,
                sacrifice_at: sacrifice_at.clone(),
                source_id,
                controller,
            });
            // CR 707.9b/9c: this seam only runs when every exception is
            // stampable onto copiable values before entry, so the body is
            // complete here and the CR 733 record can replay it exactly.
            let body_modifications = if additional_modifications.is_empty() {
                ResolvedCopyBodyModifications::NoExceptions
            } else {
                ResolvedCopyBodyModifications::Folded {
                    modifications: additional_modifications.clone(),
                    all_creature_types: state.all_creature_types.clone(),
                }
            };
            super::token::materialize_token_copy_body(
                &mut token,
                &copy_spec,
                &body_modifications,
                state.turn_number,
                entry_timestamp,
                enter_tapped.resolve(tapped),
            );
            state.liminal_entries.insert(
                token_id,
                LiminalEntry {
                    creation_frame,
                    instructions: creation_frame.map(|_| TokenCreationInstructions {
                        tapped: EtbTapState::from_seeded_tapped(tapped),
                        enters_attacking,
                    }),
                    // CR 111.1: the projection this entry will create is a
                    // token, carried as the witness the `TokenEntry` seam acts
                    // on rather than as a flag it has to trust.
                    object: crate::types::game_state::LiminalEntrant::Token(
                        crate::types::game_state::TokenProjection::materialize(token),
                    ),
                    name: name.clone(),
                    source_id,
                    controller,
                    enters_attacking,
                    attach_to: None,
                    sacrifice_at: sacrifice_at.clone(),
                    remaining_count: final_count.saturating_sub(index + 1),
                    created_ids: created_ids.clone(),
                    copy_resume: Some(copy_spec),
                    spec_resume: None,
                    enter_tapped,
                    enter_with_counters: Vec::new(),
                    kind: crate::types::game_state::LiminalEntryKind::Token,
                    replacement_applied: std::collections::HashSet::new(),
                },
            );

            let proposed = ProposedEvent::TokenEntry {
                entry_ref: token_id,
                enter_tapped,
                enter_with_counters: Vec::new(),
                applied: std::collections::HashSet::new(),
            };
            match crate::game::replacement::replace_event(state, proposed, events) {
                crate::game::replacement::ReplacementResult::Execute(event) => {
                    if state.has_post_replacement_drain() {
                        if let Some(waiting_for) =
                            crate::game::engine_replacement::apply_pending_post_replacement_effect(
                                state,
                                Some(token_id),
                                None,
                                Some(crate::types::replacements::ReplacementEvent::Moved),
                                events,
                            )
                        {
                            state.pending_liminal_entry_resume =
                                Some(PendingLiminalEntryResume::Token {
                                    source_id: token_id,
                                    player: waiting_for.acting_player().unwrap_or(controller),
                                    event: event.clone(),
                                });
                            state.waiting_for = waiting_for;
                            state.last_created_token_ids = created_ids.clone();
                            return CopyTokenApplyStatus {
                                created_ids,
                                completion: CopyTokenApplyCompletion::Paused,
                            };
                        }
                    }
                    if !super::token::commit_liminal_copy_token_entry(state, event, events) {
                        created_ids = state.last_created_token_ids.clone();
                        return CopyTokenApplyStatus {
                            created_ids,
                            completion: CopyTokenApplyCompletion::Paused,
                        };
                    }
                    // CR 707.2: this copy committed; iterate to the next token in the
                    // batch (O(1) stack). Terminal drain runs once after the loop.
                    created_ids = state.last_created_token_ids.clone();
                }
                crate::game::replacement::ReplacementResult::Prevented => {
                    state.liminal_entries.remove(&token_id);
                }
                crate::game::replacement::ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    state.last_created_token_ids = created_ids.clone();
                    return CopyTokenApplyStatus {
                        created_ids,
                        completion: CopyTokenApplyCompletion::Paused,
                    };
                }
            }
            continue;
        }

        let token_id = zones::create_object(
            state,
            CardId(0),
            token_owner,
            name.clone(),
            Zone::Battlefield,
        );

        // CR 613.7d: a copy token enters the battlefield, so it receives a
        // timestamp. Drawn before the `get_mut` (`next_timestamp` takes `&mut self`).
        let entry_timestamp = state.next_timestamp();

        let copy_spec = Box::new(CopyTokenSpec {
            values: values.clone(),
            display_source,
            printed_ref: printed_ref.clone(),
            token_image_ref: token_image_ref.clone(),
            token_art: token_art.clone(),
            extra_keywords: extra_keywords.clone(),
            additional_modifications: additional_modifications.clone(),
            tapped,
            enters_attacking,
            sacrifice_at: sacrifice_at.clone(),
            source_id,
            controller,
        });
        // CR 707.9: unlike the liminal seam, this one applies its exceptions
        // AFTER the birth through `apply_token_modifications`, which is pausable,
        // state-level, and has no resolved family of its own yet. Mark them so
        // replay refuses instead of installing a body that is missing them.
        let body_modifications = if additional_modifications.is_empty() {
            ResolvedCopyBodyModifications::NoExceptions
        } else {
            ResolvedCopyBodyModifications::DeferredToUnjournaledSeam {
                modifications: additional_modifications.clone(),
            }
        };
        let resulting_tapped = enter_tapped.resolve(tapped);
        let turn_number = state.turn_number;
        // `install_copiable_values_as_base` already seeds `loyalty`/`base_loyalty`
        // from `values.loyalty` (CR 306.5b), which is what `copied_loyalty` is,
        // so the shared body needs no separate loyalty seed.
        let created_reference = state.objects.get_mut(&token_id).map(|token| {
            super::token::materialize_token_copy_body(
                token,
                &copy_spec,
                &body_modifications,
                turn_number,
                entry_timestamp,
                resulting_tapped,
            );
            ObjectIncarnationRef::from_object(token)
        });

        // CR 707.9b/9c + CR 614.12: the consult below must see the entrant as it
        // will exist AFTER the copy exceptions, not the bare copied body.
        //
        // `materialize_token_copy_body` is a documented no-op for
        // `DeferredToUnjournaledSeam` — on this path the exceptions land later, at
        // `apply_token_modifications` — so at this point the stored object still
        // carries the UNMODIFIED copiable values. The liminal seam folds its
        // exceptions before its own consult, so without this projection the two
        // copy seams disagree about what the entrant is, and an exception that
        // adds or removes `Creature` (CR 303.4d), adds or removes the `Aura`
        // subtype, or changes color (CR 702.16c) flips the CR 303.4f/g verdict.
        // The failure mode is silent: a token that is never created.
        //
        // A PROJECTION rather than an early mutation of the stored object: the
        // exceptions must still be applied exactly once, by the seam that owns
        // them and can pause (`AddCounterOnEnter` reaches
        // `add_counter_with_replacement`), and several arms — `AddPower`,
        // `GrantAbility`, `GrantTrigger` — are not idempotent under a second pass.
        let entrant_projection = state.objects.get(&token_id).map(|token| {
            let mut projection = token.clone();
            apply_immediate_copy_token_modifications_to_object(
                &mut projection,
                &additional_modifications,
                &all_creature_types,
            );
            projection
        });

        // CR 303.4f + CR 303.4g: decide the entering Aura's host BEFORE the CR 733
        // birth is journaled (append-only, no retraction) and BEFORE the attach is
        // applied, so the CR 303.4g "if the Aura is a token, it isn't created" arm
        // can withhold the birth and the attach can never take a lower journal
        // ordinal than the birth it depends on. Same decide/act split the liminal
        // seam uses in `token::commit_liminal_token_entry_with_post_actions`.
        let hosts = match entrant_projection.as_ref() {
            Some(entrant) => {
                crate::game::zone_pipeline::entering_aura_hosts_projected(state, token_id, entrant)
            }
            None => crate::game::zone_pipeline::EnteringAuraHosts::NotApplicable,
        };
        if matches!(
            &hosts,
            crate::game::zone_pipeline::EnteringAuraHosts::Hosts { legal_targets, .. }
                if legal_targets.is_empty()
        ) {
            // CR 303.4g: this entrant is always a token on this path
            // (`materialize_token_copy_body` set `is_token`), so "it isn't created":
            // un-enter it with no birth record, no `TokenCreated`, no battlefield
            // `ZoneChanged`, no `created_ids` row, and nothing in any graveyard.
            super::token::uncreate_unentered_aura_token(state, token_id, token_owner);
            continue;
        }

        // CR 733: journal the settled copy birth, after the body borrow ends and
        // after CR 303.4g has settled that there IS a birth to journal.
        if let Some(object) = created_reference {
            let cause = state.current_or_begin_rules_execution_node();
            let command = ResolvedTokenCreationCommand {
                object,
                owner: token_owner,
                entry_timestamp,
                entry_turn: turn_number,
                body: ResolvedTokenBody::Copy {
                    copy: copy_spec.clone(),
                    modifications: body_modifications,
                },
                resulting_tapped,
                resulting_next_object_id: state.next_object_id,
                cause,
            };
            state
                .resolved_rules_journal
                .record_token_creation(command)
                .expect("resolved copy-token creation must have a live journal cause");
        }

        let tail = CopyTokenEntryTail {
            creation_frame,
            owner: token_owner,
            copy: copy_spec.clone(),
            enter_tapped,
            enter_with_counters: enter_with_counters.clone(),
            etb_counters: etb_counters.clone(),
            remaining_count: final_count.saturating_sub(index + 1),
        };

        match crate::game::zone_pipeline::apply_entering_aura_hosts(state, token_id, hosts) {
            // `NoLegalHost` is unreachable here — the empty-host arm above `continue`d.
            crate::game::zone_pipeline::EnteringAuraAttachment::NotApplicable
            | crate::game::zone_pipeline::EnteringAuraAttachment::Attached
            | crate::game::zone_pipeline::EnteringAuraAttachment::NoLegalHost => {}
            crate::game::zone_pipeline::EnteringAuraAttachment::NeedsChoice {
                controller: chooser,
                legal_targets,
            } => {
                // CR 616.1 carrier: park the WHOLE remaining entry tail — copy
                // exceptions, entry counters, entry events, and the rest of the
                // batch — behind the host choice.
                state.last_created_token_ids = created_ids.clone();
                super::counters::stash_pending_counter_additions(
                    state,
                    Vec::new(),
                    crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                        EffectKind::CopyTokenOf,
                        source_id,
                        vec![PendingCounterPostAction::ContinueCopyTokenEntryAfterAuraHost {
                            object_id: token_id,
                            tail: Box::new(tail),
                        }],
                    ),
                );
                state.waiting_for = WaitingFor::ReturnAsAuraTarget {
                    player: chooser,
                    source_id,
                    returned_id: token_id,
                    legal_targets,
                    pending_effect: Box::new(crate::types::ability::ResolvedAbility::new(
                        crate::types::ability::Effect::Attach {
                            attachment: crate::types::ability::TargetFilter::SelfRef,
                            target: crate::types::ability::TargetFilter::Any,
                            // Aura-attachment delivery: the attachment is the
                            // entering Aura; only its host is chosen here.
                            selection: crate::types::ability::AttachSelection::AtResolution {
                                count: crate::types::ability::AttachCardinality::One,
                            },
                        },
                        Vec::new(),
                        source_id,
                        chooser,
                    )),
                };
                return CopyTokenApplyStatus {
                    created_ids,
                    completion: CopyTokenApplyCompletion::Paused,
                };
            }
        }

        if !finish_non_liminal_copy_token_entry(
            state,
            token_id,
            &tail,
            &mut CopyBatchIdSink::BatchLocal(&mut created_ids),
            events,
        ) {
            let created_ids_snapshot = created_ids.clone();
            state.last_created_token_ids = created_ids_snapshot;
            return CopyTokenApplyStatus {
                created_ids,
                completion: CopyTokenApplyCompletion::Paused,
            };
        }
    }

    if liminal_immediate && creation_frame.is_none() {
        // CR 603.7 + CR 701.36a: terminal step of the iterative liminal batch —
        // set the created-token id ledger and emit the batch's final
        // EffectResolved through the pending drain, exactly as the former
        // continue_liminal_copy_token_batch None / remaining==0 arm did.
        state.waiting_for = crate::types::game_state::WaitingFor::Priority {
            player: state.active_player,
        };
        let created_ids_for_pending = state.last_created_token_ids.clone();
        if let Some(pending) = state.active_copy_token_mut() {
            pending.created_ids = created_ids_for_pending;
        }
        if state.active_copy_token().is_some() {
            drain_pending_copy_token_resolution(state, events);
        }
        created_ids = state.last_created_token_ids.clone();

        // C1: do NOT hardcode Completed. Reproduce the replaced
        // continue_liminal_copy_token_batch tail (old ~1196-1197): the whole
        // (possibly multi-batch) resolution is done only if nothing remains
        // pending or we've settled back to Priority; otherwise a later batch is
        // waiting on a CR 616.1 replacement choice, so report Paused and let the
        // caller resume rather than double-drain it (token_copy::resolve builds a
        // multi-batch VecDeque when copy_source_ids.len() > 1).
        let completion = if state.active_copy_token().is_none()
            || matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::Priority { .. }
            ) {
            CopyTokenApplyCompletion::Completed
        } else {
            CopyTokenApplyCompletion::Paused
        };
        return CopyTokenApplyStatus {
            created_ids,
            completion,
        };
    }

    CopyTokenApplyStatus {
        created_ids,
        completion: CopyTokenApplyCompletion::Completed,
    }
}

pub(crate) struct CopyTokenApplyStatus {
    pub(crate) created_ids: Vec<ObjectId>,
    pub(crate) completion: CopyTokenApplyCompletion,
}

pub(crate) enum CopyTokenApplyCompletion {
    Completed,
    Paused,
}

struct CopyTokenFinalization {
    creation_frame: Option<TokenCreationFrameId>,
    name: String,
    enters_attacking: bool,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
}

/// CR 111.1 + CR 707.2: where a finished non-liminal copy-token entry publishes
/// the id it just created.
///
/// The two live routes into [`finish_non_liminal_copy_token_entry`] differ in
/// exactly this one respect, so the difference is a named parameter rather than
/// two copies of the tail. Inline in the batch loop the running list is a local
/// the loop owns and later returns; resumed from a parked post-action that local
/// is gone, and the id must go through the guarded ledger-3 + in-flight-buffer
/// authority instead (see `token::record_last_created_copy_batch_token` for why
/// that has to be one call and not two statements).
enum CopyBatchIdSink<'a> {
    BatchLocal(&'a mut Vec<ObjectId>),
    ResumedBatch(Option<TokenCreationFrameId>),
}

impl CopyBatchIdSink<'_> {
    fn publish(&mut self, state: &mut GameState, token_id: ObjectId) {
        match self {
            CopyBatchIdSink::BatchLocal(ids) => ids.push(token_id),
            CopyBatchIdSink::ResumedBatch(creation_frame) => {
                super::token::record_last_created_copy_batch_token(
                    state,
                    *creation_frame,
                    token_id,
                );
            }
        }
    }

    /// The batch's created-id list as it stands, for the `last_created_token_ids`
    /// publication every pause inside the tail performs. On the resumed route the
    /// ledger already IS that list, so this reads it back rather than inventing one.
    fn snapshot(&self, state: &GameState) -> Vec<ObjectId> {
        match self {
            CopyBatchIdSink::BatchLocal(ids) => (**ids).clone(),
            CopyBatchIdSink::ResumedBatch(_) => state.last_created_token_ids.clone(),
        }
    }
}

/// CR 707.2 + CR 614.1c + CR 400.7: finish ONE non-liminal copy-token entry whose
/// body is already materialized and whose CR 733 birth is already journaled.
///
/// Applies the copy exceptions (CR 707.9), the entry counters (CR 306.5b copied
/// loyalty + CR 614.1c self-replacements + the creating effect's own), the
/// attacking placement (CR 508.4), the predefined-token abilities (CR 111.10),
/// and the CR 400.7 entry pair, then publishes the id through `sink`.
///
/// Returns `false` when a sub-step paused for a player choice, having parked its
/// own remainder plus the rest of the batch; the caller must report `Paused`.
fn finish_non_liminal_copy_token_entry(
    state: &mut GameState,
    token_id: ObjectId,
    tail: &CopyTokenEntryTail,
    sink: &mut CopyBatchIdSink<'_>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let token_owner = tail.owner;
    let copy_spec = &tail.copy;
    let enter_tapped = tail.enter_tapped;
    let enter_with_counters = &tail.enter_with_counters;
    let etb_counters = &tail.etb_counters;
    let remaining_count = tail.remaining_count;
    let enters_attacking = copy_spec.enters_attacking;
    let source_id = copy_spec.source_id;
    let controller = copy_spec.controller;
    let additional_modifications = copy_spec.additional_modifications.clone();
    let name = copy_spec.values.name.clone();

    let finalization = CopyTokenFinalization {
        creation_frame: tail.creation_frame,
        name: name.clone(),
        enters_attacking,
        source_id,
        controller,
    };
    if !apply_token_modifications(
        state,
        token_id,
        &finalization,
        &additional_modifications,
        events,
    ) {
        if remaining_count > 0 {
            super::counters::append_pending_counter_post_actions(
                state,
                vec![PendingCounterPostAction::ContinueCopyTokenCreation {
                    creation_frame: tail.creation_frame,
                    owner: token_owner,
                    copy: copy_spec.clone(),
                    enter_tapped,
                    enter_with_counters: enter_with_counters.clone(),
                    remaining_count,
                }],
            );
        }
        state.last_created_token_ids = sink.snapshot(state);
        return false;
    }

    finalize_copied_token(state, source_id, token_id);

    // CR 614.1c + CR 122.6a: ETB-counter replacement mutations are carried
    // on the accepted CreateToken spec, even for copy tokens whose full
    // CR 707 payload lives in `CopyTokenSpec`.
    for (counter_index, (counter_type, counter_count)) in etb_counters.iter().enumerate() {
        if *counter_count > 0
            && !super::counters::add_counter_with_replacement(
                state,
                token_owner,
                token_id,
                counter_type.clone(),
                *counter_count,
                events,
            )
        {
            state.last_created_token_ids = sink.snapshot(state);
            let remaining_counters = etb_counters[counter_index + 1..]
                .iter()
                .filter(|(_, count)| *count > 0)
                .map(|(counter_type, count)| {
                    crate::types::game_state::PendingCounterAddition::Object {
                        actor: token_owner,
                        object_id: token_id,
                        counter_type: counter_type.clone(),
                        count: *count,
                    }
                })
                .collect();
            super::counters::stash_pending_counter_additions(
                state,
                remaining_counters,
                crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                    EffectKind::CopyTokenOf,
                    source_id,
                    vec![
                        PendingCounterPostAction::FinalizeCopyTokenEntry {
                            creation_frame: tail.creation_frame,
                            object_id: token_id,
                            name: name.clone(),
                            enters_attacking,
                            source_id,
                            controller,
                        },
                        PendingCounterPostAction::ContinueCopyTokenCreation {
                            creation_frame: tail.creation_frame,
                            owner: token_owner,
                            copy: copy_spec.clone(),
                            enter_tapped,
                            enter_with_counters: enter_with_counters.clone(),
                            remaining_count,
                        },
                    ],
                ),
            );
            return false;
        }
    }

    // CR 508.4: Uses shared helper for defending player resolution.
    if enters_attacking {
        crate::game::effects::token::apply_entry_attack_instruction(
            state, token_id, source_id, controller,
        );
    }

    // CR 111.10: Predefined token abilities for known subtypes (Treasure, Food, etc.).
    //
    // PAIRED WITH THE REPLAY ARM at `token::apply_resolved_token_creation`'s
    // `ResolvedTokenBody::Copy` match arm, which must call the same
    // predefined-only injector. Unlike the liminal path — where one
    // `copy_resume.is_some()` predicate drives both the live and journaled
    // matches, so a divergence fails to compile — this branch is coupled to
    // replay by convention only. Switching it to the catalog-wide
    // `inject_resolved_token_abilities` would silently desync replay from live.
    super::token::inject_predefined_token_abilities(state, token_id);
    // Battlefield entry of a copy token: request an incremental re-derive
    // for just this token. `flush_layers` escalates to a full pass when
    // the copied object sources a continuous effect, carries a CDA, etc.
    crate::game::layers::mark_layers_entered(state, token_id);
    crate::game::restrictions::record_token_created(state, token_id);

    // CR 400.7 + CR 608.2i + CR 603.2c: route the record and the entry pair through the single
    // `from: None → Battlefield` authority so the emitted `ZoneChanged` carries this turn's
    // real zone-change index instead of the `0` placeholder. The authority performs the
    // CR 608.2i battlefield-entry bookkeeping itself, so the co-located
    // `record_battlefield_entry` call is deleted — keeping it would double-count
    // `battlefield_entries_this_turn`.
    super::token::push_committed_token_entry_events(
        state,
        tail.creation_frame,
        token_id,
        name,
        source_id,
        events,
    )
    .expect("token just created");
    sink.publish(state, token_id);
    true
}

/// CR 303.4f: resume a non-liminal copy-token entry that paused on its Aura-host
/// choice. The host is already attached by the `ReturnAsAuraTarget` answer
/// handler; everything after the attach is the shared tail.
pub(crate) fn continue_copy_token_entry_after_aura_host(
    state: &mut GameState,
    token_id: ObjectId,
    tail: CopyTokenEntryTail,
    events: &mut Vec<GameEvent>,
) -> bool {
    if !finish_non_liminal_copy_token_entry(
        state,
        token_id,
        &tail,
        &mut CopyBatchIdSink::ResumedBatch(tail.creation_frame),
        events,
    ) {
        return false;
    }
    // CR 707.2: the paused token is done; drive the rest of the batch. Publication
    // mirrors the sibling `ContinueCopyTokenCreation` resume arm exactly — a fresh
    // `created_ids` from the continuation, EXTENDED into whichever ledger owns the
    // batch — because `finish_non_liminal_copy_token_entry`'s `ResumedBatch` sink
    // has already published THIS token into both of them, and assigning the
    // continuation's list wholesale would drop it.
    if tail.remaining_count == 0 {
        return true;
    }
    let status = apply_copy_token_after_replacement(
        state,
        tail.creation_frame,
        tail.owner,
        *tail.copy,
        tail.enter_tapped,
        tail.enter_with_counters,
        tail.remaining_count,
        events,
    );
    let completion = status.completion;
    extend_copy_batch_created_ids(state, tail.creation_frame, status.created_ids);
    matches!(completion, CopyTokenApplyCompletion::Completed)
}

/// CR 111.1 + CR 707.2: fold a RESUMED copy-batch continuation's created ids into
/// whichever ledger owns the batch.
///
/// Single authority for that bulk republish, shared by every post-action arm that
/// restarts a paused batch. The two destinations are not interchangeable: while a
/// `CopyToken` frame is live its `created_ids` buffer is assigned WHOLESALE onto
/// ledger 3 at the drain, so extending ledger 3 directly would be overwritten;
/// with no frame, ledger 3 is the only destination there is.
///
/// `extend`, never assign: the id of the token whose own entry just finished has
/// already been published by `token::record_last_created_copy_batch_token`, and
/// assigning the continuation's list wholesale would drop it.
pub(crate) fn extend_copy_batch_created_ids(
    state: &mut GameState,
    creation_frame: Option<TokenCreationFrameId>,
    created_ids: Vec<ObjectId>,
) {
    let pending = match creation_frame {
        Some(id) => state.resolution_stack.token_creation_mut(id),
        None => state
            .active_copy_token_mut()
            .filter(|pending| pending.creation_id.is_none()),
    };
    if let Some(pending) = pending {
        pending.created_ids.extend(created_ids);
    } else {
        state.last_created_token_ids.extend(created_ids);
    }
}

/// CR 707.2 / CR 707.9: Complete copy-token entry and apply remaining copy
/// modifications after resuming from a counter-placement replacement pause.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_remaining_token_modifications_after_counter_pause(
    state: &mut GameState,
    creation_frame: Option<TokenCreationFrameId>,
    token_id: ObjectId,
    name: String,
    enters_attacking: bool,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
    remaining_modifications: Vec<ContinuousModification>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let finalization = CopyTokenFinalization {
        creation_frame,
        name: name.clone(),
        enters_attacking,
        source_id,
        controller,
    };
    if !apply_token_modifications(
        state,
        token_id,
        &finalization,
        &remaining_modifications,
        events,
    ) {
        return false;
    }
    finalize_copied_token(state, source_id, token_id);
    if enters_attacking {
        crate::game::effects::token::apply_entry_attack_instruction(
            state, token_id, source_id, controller,
        );
    }
    super::token::inject_predefined_token_abilities(state, token_id);
    crate::game::layers::mark_layers_entered(state, token_id);
    crate::game::restrictions::record_token_created(state, token_id);
    // CR 400.7 + CR 608.2i + CR 603.2c: route the record and the entry pair through the single
    // `from: None → Battlefield` authority so the emitted `ZoneChanged` carries this turn's real
    // zone-change index instead of the `0` placeholder. The authority performs the CR 608.2i
    // battlefield-entry bookkeeping itself, so the co-located `record_battlefield_entry` call is
    // deleted — keeping it would double-count `battlefield_entries_this_turn`.
    //
    // OBJECT-GONE: one of four counter-pause / deferred resume routes with this shape, all covered
    // by the same predicate inside `push_committed_token_entry_events` — it gates `TokenCreated` on
    // the authority's `None` verdict, so a vanished token cannot put a live creation event on the
    // wire with no `created_tokens_this_turn` row behind it. MEASURED with the predicate deleted,
    // token removed: `(TokenCreated=1, created_tokens_this_turn=0, last_created_token_ids=1)` —
    // exactly the disagreement
    // `a_vanished_counter_paused_token_reports_neither_creation_event_nor_ledger_row` forbids. The
    // anaphora slot below carries the SAME predicate through
    // `record_last_created_copy_batch_token`, which is the third ledger of the triple; without it
    // the gone path reads `(0, 0, 1)`. That call owns BOTH of the slot's destinations — ledger 3
    // and this batch's `created_ids`, which `drain_pending_copy_token_resolution` assigns wholesale
    // back onto ledger 3 — because a separate buffer push republished the withheld id and clobbered
    // the guarded list on top of it.
    super::token::push_committed_token_entry_events(
        state,
        creation_frame,
        token_id,
        name,
        source_id,
        events,
    );
    super::token::record_last_created_copy_batch_token(state, creation_frame, token_id);
    true
}

/// CR 707.2 + CR 707.9: Apply non-keyword `, except <body>` modifications to
/// a synthesized token. Tokens are created with copiable values baked in, so
/// each modification mutates BOTH the layered view (`card_types`,
/// `keywords`, etc.) AND the base view (`base_card_types`, `base_keywords`)
/// directly — there is no "before exception" state to layer over the way a
/// `BecomeCopy` modification layers over an existing object.
///
/// Variants consumed here:
/// - `RemoveSupertype` / `AddSupertype` — Miirym, Sentinel Wyrm; Sarkhan-class.
/// - `AddCounterOnEnter` — Spark Double-class. Counter placed via the shared
///   `counters::add_counter_with_replacement` primitive (which handles
///   replacements such as Doubling Season).
/// - `SetName` — copy-name override (rare for token-copy, harmless if present).
/// - `AddType` / `RemoveType` / `AddSubtype` / `RemoveSubtype` — type
///   exception support for token-copy (compose with type-modifying except
///   bodies that share grammar with `BecomeCopy`).
/// - `SetCardTypes` — Myrkul, Lord of Bones: "it's an enchantment and loses
///   all other card types" replaces the copied core card-type set (CR 613.1d).
/// - `AddKeyword` — dual-path: keywords in the typed `extra_keywords` channel
///   are applied earlier in the resolver, and an `AddKeyword` that instead lands
///   in `additional_modifications` (e.g. an "except it has menace" body, or a
///   keyword adjacent to a quoted-ability grant) is applied here too, on the
///   `AddKeyword` arm below (CR 707.9a). Both routes add to the copiable keyword
///   set idempotently.
///
/// Modifications not relevant to token-copy semantics (e.g. `CopyValues`,
/// `ChangeController`, dynamic P/T) are skipped silently — they have no
/// meaningful "stamp at creation" interpretation. A future card with such
/// an except body will surface as an unimplemented modification, which is
/// strictly better than silently mutating the token incorrectly.
fn apply_token_modifications(
    state: &mut GameState,
    token_id: ObjectId,
    finalization: &CopyTokenFinalization,
    modifications: &[ContinuousModification],
    events: &mut Vec<GameEvent>,
) -> bool {
    for (index, modification) in modifications.iter().enumerate() {
        match modification {
            // CR 205.4 + CR 707.9b: "the token isn't legendary" (Miirym class).
            ContinuousModification::RemoveSupertype { supertype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.supertypes.retain(|s| s != supertype);
                    token.base_card_types.supertypes.retain(|s| s != supertype);
                }
            }
            // CR 205.4 + CR 707.9d: "it's <supertype> in addition to its other types".
            ContinuousModification::AddSupertype { supertype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.supertypes.contains(supertype) {
                        token.card_types.supertypes.push(*supertype);
                    }
                    if !token.base_card_types.supertypes.contains(supertype) {
                        token.base_card_types.supertypes.push(*supertype);
                    }
                }
            }
            // CR 122.1 + CR 614.1c: Counter at creation, optionally gated by
            // the resolved core type. Read core types from the just-stamped
            // `card_types` (already includes any AddType/RemoveType applied
            // earlier in this loop) before placing the counter.
            ContinuousModification::AddCounterOnEnter {
                counter_type,
                count,
                if_type,
            } => {
                let counter_actor = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let n = resolve_quantity(state, count, counter_actor, token_id).max(0) as u32;
                if n == 0 {
                    continue;
                }
                let gate_passes = match if_type {
                    None => true,
                    Some(t) => state
                        .objects
                        .get(&token_id)
                        .map(|obj| obj.card_types.core_types.contains(t))
                        .unwrap_or(false),
                };
                if !gate_passes {
                    continue;
                }
                if !super::counters::add_counter_with_replacement(
                    state,
                    counter_actor,
                    token_id,
                    counter_type.clone(),
                    n,
                    events,
                ) {
                    super::counters::stash_pending_counter_post_actions(
                        state,
                        EffectKind::CopyTokenOf,
                        finalization.source_id,
                        vec![
                            PendingCounterPostAction::ApplyCopyTokenModificationsAndFinalize {
                                creation_frame: finalization.creation_frame,
                                object_id: token_id,
                                name: finalization.name.clone(),
                                enters_attacking: finalization.enters_attacking,
                                source_id: finalization.source_id,
                                controller: finalization.controller,
                                remaining_modifications: modifications[index + 1..].to_vec(),
                            },
                        ],
                    );
                    return false;
                }
            }
            // CR 707.9b: Name override applied at copy time.
            ContinuousModification::SetName { name } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.name = name.clone();
                    token.base_name = name.clone();
                }
            }
            // CR 205.1a: Type/subtype additions/removals as copy exceptions.
            ContinuousModification::AddType { core_type } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.core_types.contains(core_type) {
                        token.card_types.core_types.push(*core_type);
                    }
                    if !token.base_card_types.core_types.contains(core_type) {
                        token.base_card_types.core_types.push(*core_type);
                    }
                }
            }
            // CR 205.1a: removing a card type removes its correlated subtypes too;
            // applied to both live and base types so the loss is part of the
            // token's copiable values (CR 707.9b).
            ContinuousModification::RemoveType { core_type } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    remove_core_type(&mut token.card_types, *core_type);
                    remove_core_type(&mut token.base_card_types, *core_type);
                }
            }
            ContinuousModification::AddSubtype { subtype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.card_types.subtypes.iter().any(|s| s == subtype) {
                        token.card_types.subtypes.push(subtype.clone());
                    }
                    if !token.base_card_types.subtypes.iter().any(|s| s == subtype) {
                        token.base_card_types.subtypes.push(subtype.clone());
                    }
                }
            }
            ContinuousModification::RemoveSubtype { subtype } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.subtypes.retain(|s| s != subtype);
                    token.base_card_types.subtypes.retain(|s| s != subtype);
                }
            }
            // CR 707.9b + CR 613.1d: a copy exception with no "in addition"
            // carve-out replaces the copied creature subtypes (CR 707.9d). The
            // wiped set becomes part of the token's copiable values, so apply it
            // to both live and base subtype lists.
            ContinuousModification::RemoveAllSubtypes { set } => {
                let all_creature_types = &state.all_creature_types;
                let objects = &mut state.objects;
                if let Some(token) = objects.get_mut(&token_id) {
                    remove_subtype_set(&mut token.card_types.subtypes, *set, all_creature_types);
                    remove_subtype_set(
                        &mut token.base_card_types.subtypes,
                        *set,
                        all_creature_types,
                    );
                }
            }
            // CR 205.1a + CR 613.1d + CR 707.9d: "it's an enchantment and loses
            // all other card types" (Myrkul, Lord of Bones) REPLACES the copied
            // card's core card-type set. Supertypes (Legendary) are retained;
            // subtypes are filtered through the shared `subtype_matches_core_types`
            // rule so this baked path keeps exactly the subtypes the layered
            // `SetCardTypes` arm would (uncorrelated noncreature subtypes drop).
            // Stamped into both live and base card types so the override is part
            // of the token's copiable values (CR 707.9b).
            ContinuousModification::SetCardTypes { core_types } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.card_types.core_types = core_types.clone();
                    token.base_card_types.core_types = core_types.clone();
                    let keep = |subtype: &String| {
                        crate::game::layers::subtype_matches_core_types(subtype, core_types)
                    };
                    token.card_types.subtypes.retain(|s| keep(s));
                    token.base_card_types.subtypes.retain(|s| keep(s));
                }
            }
            // CR 707.9b + CR 613.1e: a copy exception that sets color (no
            // "in addition to its other colors" carve-out, CR 707.9d) replaces
            // the copied color. The result becomes part of the token's copiable
            // values, so set both live and base color.
            ContinuousModification::SetColor { colors } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.color = colors.clone();
                    token.base_color = colors.clone();
                }
            }
            // CR 707.9 + CR 202.1b: "except it has no mana cost" — strip the
            // copied mana cost so the token's mana value is 0 (Embalm
            // CR 702.128a, Eternalize CR 702.129a). Set both live and base so
            // the override is part of the token's copiable values.
            ContinuousModification::RemoveManaCost => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.mana_cost = crate::types::mana::ManaCost::NoCost;
                    token.base_mana_cost = crate::types::mana::ManaCost::NoCost;
                }
            }
            // CR 707.9b + CR 613.1e: a copy exception that adds color
            // ("in addition to its other colors") becomes part of the token's
            // copiable values without removing the copied color.
            ContinuousModification::AddColor { color } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.color.contains(color) {
                        token.color.push(*color);
                    }
                    if !token.base_color.contains(color) {
                        token.base_color.push(*color);
                    }
                }
            }
            // CR 707.9b: "except it's 1/1" — set base and live P/T so the
            // override persists through layer resets. Used by Offspring
            // (CR 702.175a) and Saw in Half.
            ContinuousModification::SetPower { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_power = Some(*value);
                    token.power = Some(*value);
                    token.layer_base_power = Some(*value);
                }
            }
            ContinuousModification::SetToughness { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_toughness = Some(*value);
                    token.toughness = Some(*value);
                    token.layer_base_toughness = Some(*value);
                }
            }
            // CR 707.9b: fixed additive P/T exceptions are baked into the
            // token's copiable values by updating both base and live P/T.
            ContinuousModification::AddPower { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_power = token.base_power.map(|p| p + *value);
                    token.power = token.power.map(|p| p + *value);
                    token.layer_base_power = token.layer_base_power.map(|p| p + *value);
                }
            }
            ContinuousModification::AddToughness { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_toughness = token.base_toughness.map(|t| t + *value);
                    token.toughness = token.toughness.map(|t| t + *value);
                    token.layer_base_toughness = token.layer_base_toughness.map(|t| t + *value);
                }
            }
            // CR 707.9b: "except its base power and toughness are each equal
            // to half [X]" (Saw in Half). Dynamic quantity resolved at
            // creation time and stamped as base P/T.
            ContinuousModification::SetPowerDynamic { value } => {
                let controller = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let val = resolve_quantity(state, value, controller, token_id);
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_power = Some(val);
                    token.power = Some(val);
                    token.layer_base_power = Some(val);
                }
            }
            ContinuousModification::SetToughnessDynamic { value } => {
                let controller = state
                    .objects
                    .get(&token_id)
                    .map(|o| o.controller)
                    .unwrap_or(crate::types::player::PlayerId(0));
                let val = resolve_quantity(state, value, controller, token_id);
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_toughness = Some(val);
                    token.toughness = Some(val);
                    token.layer_base_toughness = Some(val);
                }
            }
            // CR 707.9b + CR 306.5b/c: Starting-loyalty exceptions are already
            // consumed before loyalty counters are seeded, but apply the live
            // fields idempotently so a paused/resumed modification sequence
            // keeps the stamped token coherent.
            ContinuousModification::SetStartingLoyalty { value } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.base_loyalty = Some(*value);
                    token.loyalty = Some(*value);
                    token.base_printed_loyalty = Some(PrintedLoyalty::Fixed(*value));
                    token.printed_loyalty = Some(PrintedLoyalty::Fixed(*value));
                }
            }
            // CR 707.9a + CR 603.1: "except it has \"<triggered ability>\""
            // (Chandra, Flameshaper [+1]: "…and \"At the beginning of the end
            // step, sacrifice this token.\""). The granted trigger becomes part
            // of the copy's copiable values, so stamp it onto both the live and
            // base trigger sets. `SelfRef`/`This`-anchored triggers in the
            // grant resolve against the token because they fire with the token
            // as their source.
            ContinuousModification::GrantTrigger { trigger } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    token.push_printed_trigger((**trigger).clone());
                }
            }
            // CR 707.9a: "except it has \"<activated/static ability>\"" — the
            // granted ability is part of the copy's copiable values. Mirrors the
            // GrantTrigger arm for the `obj.abilities` list.
            ContinuousModification::GrantAbility { definition } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    Arc::make_mut(&mut token.abilities).push((**definition).clone());
                    Arc::make_mut(&mut token.base_abilities).push((**definition).clone());
                }
            }
            // CR 707.9a + CR 604.1: quoted static text in a copy exception is
            // also part of the token's copiable values. Mirror predefined token
            // rules text by carrying static-rule modifications on a self static.
            ContinuousModification::GrantStaticAbility { .. }
            | ContinuousModification::AddStaticMode { .. } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    let static_def = StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(vec![modification.clone()]);
                    Arc::make_mut(&mut token.base_static_definitions).push(static_def.clone());
                    token.static_definitions.push(static_def);
                }
            }
            // CR 707.9a + CR 702: a keyword granted via an except body that
            // landed in `additional_modifications` (rather than `extra_keywords`)
            // — e.g. a keyword adjacent to a quoted-ability grant. Apply it to
            // the copiable keyword set, idempotently, mirroring `extra_keywords`.
            ContinuousModification::AddKeyword { keyword } => {
                if let Some(token) = state.objects.get_mut(&token_id) {
                    if !token.keywords.contains(keyword) {
                        token.keywords.push(keyword.clone());
                    }
                    if !token.base_keywords.contains(keyword) {
                        token.base_keywords.push(keyword.clone());
                    }
                }
            }
            // Other layered-only modifications (CopyValues, ChangeController,
            // etc.) are intentionally skipped — their "stamp at copy time"
            // interpretation is ambiguous, and a future except body needing
            // them should route through the BecomeCopy layered path instead.
            _ => {}
        }
    }
    true
}

/// CR 707.2 + CR 707.9: classify copy exceptions that can be stamped into a
/// liminal token's copiable values before replacement consultation.
pub(crate) fn copy_token_modifications_are_liminal_immediate(
    modifications: &[ContinuousModification],
) -> bool {
    // CR 707.2 + CR 707.9: only copy modifications that can be stamped onto
    // copiable values before replacement consultation may use the liminal entry
    // path. Dynamic P/T and ETB counters need the committed object/replacement
    // pipeline instead.
    modifications.iter().all(|modification| {
        !matches!(
            modification,
            ContinuousModification::AddCounterOnEnter { .. }
                | ContinuousModification::SetPowerDynamic { .. }
                | ContinuousModification::SetToughnessDynamic { .. }
        )
    })
}

/// CR 707.2 + CR 707.9: apply immediate copy exceptions to a liminal token's
/// copiable values so self as-enters replacements consult the final shape.
pub(crate) fn apply_immediate_copy_token_modifications_to_object(
    token: &mut GameObject,
    modifications: &[ContinuousModification],
    all_creature_types: &[String],
) {
    // CR 707.2 + CR 707.9: apply immediate "except" copy modifications to the
    // not-yet-committed token's copiable characteristics so self as-enters
    // replacements consult the final copied shape.
    for modification in modifications {
        match modification {
            ContinuousModification::RemoveSupertype { supertype } => {
                token.card_types.supertypes.retain(|s| s != supertype);
                token.base_card_types.supertypes.retain(|s| s != supertype);
            }
            ContinuousModification::AddSupertype { supertype } => {
                if !token.card_types.supertypes.contains(supertype) {
                    token.card_types.supertypes.push(*supertype);
                }
                if !token.base_card_types.supertypes.contains(supertype) {
                    token.base_card_types.supertypes.push(*supertype);
                }
            }
            ContinuousModification::SetName { name } => {
                token.name = name.clone();
                token.base_name = name.clone();
            }
            ContinuousModification::AddType { core_type } => {
                if !token.card_types.core_types.contains(core_type) {
                    token.card_types.core_types.push(*core_type);
                }
                if !token.base_card_types.core_types.contains(core_type) {
                    token.base_card_types.core_types.push(*core_type);
                }
            }
            // CR 205.1a + CR 707.9b: same as the layered `RemoveType` arm.
            ContinuousModification::RemoveType { core_type } => {
                remove_core_type(&mut token.card_types, *core_type);
                remove_core_type(&mut token.base_card_types, *core_type);
            }
            ContinuousModification::AddSubtype { subtype } => {
                if !token.card_types.subtypes.iter().any(|s| s == subtype) {
                    token.card_types.subtypes.push(subtype.clone());
                }
                if !token.base_card_types.subtypes.iter().any(|s| s == subtype) {
                    token.base_card_types.subtypes.push(subtype.clone());
                }
            }
            ContinuousModification::RemoveSubtype { subtype } => {
                token.card_types.subtypes.retain(|s| s != subtype);
                token.base_card_types.subtypes.retain(|s| s != subtype);
            }
            ContinuousModification::RemoveAllSubtypes { set } => {
                remove_subtype_set(&mut token.card_types.subtypes, *set, all_creature_types);
                remove_subtype_set(
                    &mut token.base_card_types.subtypes,
                    *set,
                    all_creature_types,
                );
            }
            ContinuousModification::SetCardTypes { core_types } => {
                token.card_types.core_types = core_types.clone();
                token.base_card_types.core_types = core_types.clone();
                let keep = |subtype: &String| {
                    crate::game::layers::subtype_matches_core_types(subtype, core_types)
                };
                token.card_types.subtypes.retain(|s| keep(s));
                token.base_card_types.subtypes.retain(|s| keep(s));
            }
            ContinuousModification::SetColor { colors } => {
                token.color = colors.clone();
                token.base_color = colors.clone();
            }
            ContinuousModification::RemoveManaCost => {
                token.mana_cost = crate::types::mana::ManaCost::NoCost;
                token.base_mana_cost = crate::types::mana::ManaCost::NoCost;
            }
            ContinuousModification::AddColor { color } => {
                if !token.color.contains(color) {
                    token.color.push(*color);
                }
                if !token.base_color.contains(color) {
                    token.base_color.push(*color);
                }
            }
            ContinuousModification::SetPower { value } => {
                token.base_power = Some(*value);
                token.power = Some(*value);
                token.layer_base_power = Some(*value);
            }
            ContinuousModification::SetToughness { value } => {
                token.base_toughness = Some(*value);
                token.toughness = Some(*value);
                token.layer_base_toughness = Some(*value);
            }
            ContinuousModification::AddPower { value } => {
                token.base_power = token.base_power.map(|p| p + *value);
                token.power = token.power.map(|p| p + *value);
                token.layer_base_power = token.layer_base_power.map(|p| p + *value);
            }
            ContinuousModification::AddToughness { value } => {
                token.base_toughness = token.base_toughness.map(|t| t + *value);
                token.toughness = token.toughness.map(|t| t + *value);
                token.layer_base_toughness = token.layer_base_toughness.map(|t| t + *value);
            }
            ContinuousModification::SetStartingLoyalty { value } => {
                token.base_loyalty = Some(*value);
                token.loyalty = Some(*value);
                token.base_printed_loyalty = Some(PrintedLoyalty::Fixed(*value));
                token.printed_loyalty = Some(PrintedLoyalty::Fixed(*value));
            }
            ContinuousModification::GrantTrigger { trigger } => {
                token.push_printed_trigger((**trigger).clone());
            }
            ContinuousModification::GrantAbility { definition } => {
                Arc::make_mut(&mut token.abilities).push((**definition).clone());
                Arc::make_mut(&mut token.base_abilities).push((**definition).clone());
            }
            ContinuousModification::GrantStaticAbility { .. }
            | ContinuousModification::AddStaticMode { .. } => {
                let static_def = StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![modification.clone()]);
                Arc::make_mut(&mut token.base_static_definitions).push(static_def.clone());
                token.static_definitions.push(static_def);
            }
            ContinuousModification::AddKeyword { keyword } => {
                if !token.keywords.contains(keyword) {
                    token.keywords.push(keyword.clone());
                }
                if !token.base_keywords.contains(keyword) {
                    token.base_keywords.push(keyword.clone());
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn copy_starting_loyalty_override(
    modifications: &[ContinuousModification],
) -> Option<u32> {
    modifications.iter().rev().find_map(|modification| {
        if let ContinuousModification::SetStartingLoyalty { value } = modification {
            Some(*value)
        } else {
            None
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::game_object::DisplaySource;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, AdditionalCostPaymentSource, ContinuousModification,
        ControllerRef, CostPaidObjectSnapshot, Effect, FilterProp, ObjectScope, PtValue,
        QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition, ReplacementMode,
        RoundingMode, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card::PrintedCardRef;
    use crate::types::card_type::{CardType, CoreType, SubtypeSet, Supertype};
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{ObjectId, TrackedSetId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolution::{FrameKind, ResolutionStateWire};
    use crate::types::triggers::TriggerMode;

    /// CR 707.9b + CR 707.9d: a copy token whose exception sets P/T, replaces
    /// color, and replaces creature subtypes (The Scarab God shape) stamps each
    /// characteristic onto both the live and base (copiable) values of the
    /// synthesized token.
    #[test]
    fn copy_token_exceptions_stamp_pt_color_and_subtype() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec![
            "Human".to_string(),
            "Soldier".to_string(),
            "Zombie".to_string(),
        ];
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Elite Vanguard".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(1);
            source.power = Some(2);
            source.toughness = Some(1);
            source.base_color = vec![ManaColor::White];
            source.color = vec![ManaColor::White];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Soldier".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::SetPower { value: 4 },
                    ContinuousModification::SetToughness { value: 4 },
                    ContinuousModification::SetColor {
                        colors: vec![ManaColor::Black],
                    },
                    ContinuousModification::RemoveAllSubtypes {
                        set: SubtypeSet::Creature,
                    },
                    ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Zombie".to_string(),
                    },
                ],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(4));
        assert_eq!(token.color, vec![ManaColor::Black]);
        assert!(token.card_types.subtypes.contains(&"Zombie".to_string()));
        assert!(!token.card_types.subtypes.contains(&"Human".to_string()));
        assert!(!token.card_types.subtypes.contains(&"Soldier".to_string()));
        assert_eq!(token.base_power, Some(4));
        assert_eq!(token.base_toughness, Some(4));
        assert_eq!(token.base_color, vec![ManaColor::Black]);
        assert!(token
            .base_card_types
            .subtypes
            .contains(&"Zombie".to_string()));
        assert!(!token
            .base_card_types
            .subtypes
            .contains(&"Human".to_string()));
        assert!(!token
            .base_card_types
            .subtypes
            .contains(&"Soldier".to_string()));
    }

    /// CR 205.1a + CR 613.1d + CR 707.9d: Myrkul, Lord of Bones — "create a
    /// token that's a copy of that card, except it's an enchantment and loses
    /// all other card types." `SetCardTypes` replaces the copied creature's
    /// core types with `[Enchantment]` (no longer a creature), while supertypes
    /// (Legendary) survive. Subtype retention follows the shared
    /// `subtype_matches_core_types` rule used by the layered path: a noncreature
    /// subtype not correlated to the new core types (here the artifact subtype
    /// "Equipment") drops, keeping both applications consistent. Applied to both
    /// live and base (copiable) card types.
    #[test]
    fn copy_token_set_card_types_replaces_core_types_and_drops_uncorrelated_subtype() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dying God".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(7);
            source.base_toughness = Some(5);
            source.power = Some(7);
            source.toughness = Some(5);
            source.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact, CoreType::Creature],
                subtypes: vec!["Equipment".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::SetCardTypes {
                    core_types: vec![CoreType::Enchantment],
                }],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        // Core types replaced: enchantment only, no longer a creature or artifact.
        assert_eq!(token.card_types.core_types, vec![CoreType::Enchantment]);
        assert_eq!(
            token.base_card_types.core_types,
            vec![CoreType::Enchantment]
        );
        // CR 205.1a: the "Equipment" artifact subtype is no longer correlated, so it drops.
        assert!(!token.card_types.subtypes.contains(&"Equipment".to_string()));
        assert!(!token
            .base_card_types
            .subtypes
            .contains(&"Equipment".to_string()));
        // Supertypes are unaffected by a card-type replacement.
        assert!(token.card_types.supertypes.contains(&Supertype::Legendary));
        assert!(token
            .base_card_types
            .supertypes
            .contains(&Supertype::Legendary));
    }

    /// CR 205.1a + CR 707.9b: a baked `RemoveType { Creature }` copy exception
    /// removes the creature types with the Creature type, in both the live and
    /// the base (copiable) card types. Kindred keeps the creature set
    /// (CR 308.2).
    #[test]
    fn copy_token_remove_type_creature_drops_creature_types_in_card_and_base_types() {
        for (core_types, subtypes, expected_core, expected_subtypes) in [
            (
                vec![CoreType::Artifact, CoreType::Creature],
                vec!["Heartwood", "Bear"],
                vec![CoreType::Artifact],
                vec!["Heartwood"],
            ),
            (
                vec![CoreType::Artifact, CoreType::Creature],
                vec!["Vibranium", "Bear"],
                vec![CoreType::Artifact],
                vec!["Vibranium"],
            ),
            (
                vec![CoreType::Artifact, CoreType::Creature],
                vec!["Equipment", "Bear"],
                vec![CoreType::Artifact],
                vec!["Equipment"],
            ),
            (
                vec![CoreType::Kindred, CoreType::Creature],
                vec!["Eldrazi"],
                vec![CoreType::Kindred],
                vec!["Eldrazi"],
            ),
        ] {
            let mut state = GameState::new_two_player(42);
            state.all_creature_types = vec!["Bear".to_string(), "Eldrazi".to_string()];
            let source_id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Source".to_string(),
                Zone::Battlefield,
            );
            {
                let source = state.objects.get_mut(&source_id).unwrap();
                source.base_card_types = CardType {
                    supertypes: vec![],
                    core_types: core_types.clone(),
                    subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
                };
                source.card_types = source.base_card_types.clone();
            }

            let mut events = Vec::new();
            let ability = ResolvedAbility::new(
                Effect::CopyTokenOf {
                    target: TargetFilter::SelfRef,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    extra_keywords: vec![],
                    additional_modifications: vec![ContinuousModification::RemoveType {
                        core_type: CoreType::Creature,
                    }],
                },
                vec![],
                source_id,
                PlayerId(0),
            );
            resolve(&mut state, &ability, &mut events).unwrap();

            let token = state
                .objects
                .get(&ObjectId(state.next_object_id - 1))
                .unwrap();
            let expected_subtypes: Vec<String> =
                expected_subtypes.iter().map(|s| s.to_string()).collect();
            // Reach guard: the token exists, lost Creature and kept the expected type.
            assert_eq!(token.card_types.core_types, expected_core);
            assert_eq!(token.base_card_types.core_types, expected_core);
            assert_eq!(token.card_types.subtypes, expected_subtypes);
            assert_eq!(token.base_card_types.subtypes, expected_subtypes);
        }
    }

    /// CR 205.1a + CR 707.2 + CR 707.9b: the immediate (liminal token) copy
    /// exception path removes creature types with the Creature type too.
    #[test]
    fn immediate_copy_token_remove_type_creature_drops_creature_types() {
        for (core_types, subtypes, expected_core, expected_subtypes) in [
            (
                vec![CoreType::Artifact, CoreType::Creature],
                vec!["Heartwood", "Bear"],
                vec![CoreType::Artifact],
                vec!["Heartwood"],
            ),
            (
                vec![CoreType::Artifact, CoreType::Creature],
                vec!["Vibranium", "Bear"],
                vec![CoreType::Artifact],
                vec!["Vibranium"],
            ),
            (
                vec![CoreType::Artifact, CoreType::Creature],
                vec!["Equipment", "Bear"],
                vec![CoreType::Artifact],
                vec!["Equipment"],
            ),
            (
                vec![CoreType::Kindred, CoreType::Creature],
                vec!["Eldrazi"],
                vec![CoreType::Kindred],
                vec!["Eldrazi"],
            ),
        ] {
            let mut token = GameObject::new(
                ObjectId(900),
                CardId(900),
                PlayerId(0),
                "Liminal Token".to_string(),
                Zone::Battlefield,
            );
            token.base_card_types = CardType {
                supertypes: vec![],
                core_types: core_types.clone(),
                subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
            };
            token.card_types = token.base_card_types.clone();

            apply_immediate_copy_token_modifications_to_object(
                &mut token,
                &[ContinuousModification::RemoveType {
                    core_type: CoreType::Creature,
                }],
                &["Bear".to_string(), "Eldrazi".to_string()],
            );

            let expected_subtypes: Vec<String> =
                expected_subtypes.iter().map(|s| s.to_string()).collect();
            assert_eq!(token.card_types.core_types, expected_core);
            assert_eq!(token.base_card_types.core_types, expected_core);
            assert_eq!(token.card_types.subtypes, expected_subtypes);
            assert_eq!(token.base_card_types.subtypes, expected_subtypes);
        }
    }

    #[test]
    fn copy_token_artifact_subtypes_follow_owner_in_both_baked_paths() {
        // CR 205.1a/205.3g + CR 707.9b: synthetic copy exceptions test the
        // shared primitive, without claiming a printed token-animation pair.
        for subtype in ["Heartwood", "Vibranium"] {
            for (modification, core, expected_subtype) in [
                (
                    ContinuousModification::RemoveType {
                        core_type: CoreType::Creature,
                    },
                    CoreType::Artifact,
                    subtype,
                ),
                (
                    ContinuousModification::RemoveType {
                        core_type: CoreType::Artifact,
                    },
                    CoreType::Creature,
                    "Bear",
                ),
                (
                    ContinuousModification::SetCardTypes {
                        core_types: vec![CoreType::Artifact],
                    },
                    CoreType::Artifact,
                    subtype,
                ),
                (
                    ContinuousModification::SetCardTypes {
                        core_types: vec![CoreType::Creature],
                    },
                    CoreType::Creature,
                    "Bear",
                ),
            ] {
                let mut state = GameState::new_two_player(42);
                state.all_creature_types = vec!["Bear".into(), "Elf".into()];
                let source_id = create_object(
                    &mut state,
                    CardId(1),
                    PlayerId(0),
                    "Artifact copy source".into(),
                    Zone::Battlefield,
                );
                let source = state.objects.get_mut(&source_id).unwrap();
                source.base_card_types = CardType {
                    supertypes: vec![Supertype::Legendary],
                    core_types: vec![CoreType::Artifact, CoreType::Creature],
                    subtypes: vec![subtype.into(), "Bear".into()],
                };
                source.card_types = source.base_card_types.clone();
                let mut liminal = source.clone();
                let ability = ResolvedAbility::new(
                    Effect::CopyTokenOf {
                        target: TargetFilter::SelfRef,
                        owner: TargetFilter::Controller,
                        source_filter: None,
                        enters_attacking: false,
                        tapped: false,
                        count: QuantityExpr::Fixed { value: 1 },
                        extra_keywords: vec![],
                        additional_modifications: vec![modification.clone()],
                    },
                    vec![],
                    source_id,
                    PlayerId(0),
                );
                let mut events = Vec::new();
                resolve(&mut state, &ability, &mut events).unwrap();
                let token_id = ObjectId(state.next_object_id - 1);
                assert_ne!(token_id, source_id);
                let token = &state.objects[&token_id];
                assert!(token.is_token, "reach: CopyTokenOf created a token");
                assert_eq!(token.name, "Artifact copy source");
                assert_eq!(
                    state.objects[&source_id].card_types.subtypes,
                    vec![subtype.to_string(), "Bear".to_string()]
                );
                apply_immediate_copy_token_modifications_to_object(
                    &mut liminal,
                    &[modification],
                    &state.all_creature_types,
                );
                for object in [token, &liminal] {
                    assert_eq!(object.card_types.core_types, vec![core]);
                    assert_eq!(object.base_card_types.core_types, vec![core]);
                    assert_eq!(
                        object.card_types.subtypes,
                        vec![expected_subtype.to_string()]
                    );
                    assert_eq!(
                        object.base_card_types.subtypes,
                        vec![expected_subtype.to_string()]
                    );
                    assert_eq!(object.card_types.supertypes, vec![Supertype::Legendary]);
                    assert_eq!(
                        object.base_card_types.supertypes,
                        vec![Supertype::Legendary]
                    );
                }
            }
        }
    }

    #[test]
    fn copy_token_of_self_creates_copy() {
        let mut state = GameState::new_two_player(42);

        // Create a creature to copy
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mist-Syndicate Naga".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(1);
            source.power = Some(3);
            source.toughness = Some(1);
            source.base_color = vec![ManaColor::Blue];
            source.color = vec![ManaColor::Blue];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Snake".to_string(), "Ninja".to_string()],
            };
            source.card_types = source.base_card_types.clone();
            source.base_keywords = vec![Keyword::Ninjutsu(Default::default())];
            source.keywords = source.base_keywords.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Find the token (it's the newest object)
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        assert_eq!(token.name, "Mist-Syndicate Naga");
        assert_eq!(token.power, Some(3));
        assert_eq!(token.toughness, Some(1));
        assert_eq!(token.color, vec![ManaColor::Blue]);
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.contains(&"Snake".to_string()));
        assert!(token.is_token);
        assert!(token.zone == Zone::Battlefield);
        assert!(state.layers_dirty.is_dirty());
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::TokenCreated { name, .. } if name == "Mist-Syndicate Naga")
        ));
        // Verify record_token_created was called
        assert!(
            state
                .players_who_created_token_this_turn
                .contains(&PlayerId(0)),
            "should record token creation"
        );
    }

    /// CR 707.2 + CR 111.10: a copy token sourced from a card with only a
    /// runtime name, no printed ref, and no source-related token ids remains a
    /// card-display copy even if the copied body matches a catalog token preset.
    #[test]
    fn copy_token_of_name_only_card_does_not_bind_catalog_preset() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fanatic of Rhonas".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.display_source = DisplaySource::Card;
            source.base_power = Some(4);
            source.base_toughness = Some(4);
            source.power = Some(4);
            source.toughness = Some(4);
            source.base_color = vec![ManaColor::Black];
            source.color = vec![ManaColor::Black];
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![
                    "Zombie".to_string(),
                    "Snake".to_string(),
                    "Druid".to_string(),
                ],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert_eq!(token.name, "Fanatic of Rhonas");
        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(4));
        assert_eq!(token.color, vec![ManaColor::Black]);
        assert_eq!(token.card_types.core_types, vec![CoreType::Creature]);
        assert_eq!(
            token.card_types.subtypes,
            vec![
                "Zombie".to_string(),
                "Snake".to_string(),
                "Druid".to_string()
            ]
        );
        assert_eq!(token.display_source, DisplaySource::Card);
        assert_eq!(token.token_image_ref, None);
    }

    /// CR 614.1a + CR 707.2: A token-count-doubling replacement (Doubling
    /// Season / Adrix and Nev / Parallel Lives / Anointed Procession / Mondrak)
    /// applies to a token that's a *copy* of a permanent, exactly as it applies
    /// to a predefined `Effect::Token`. Such doublers are CR 614.1a replacement
    /// effects that modify the number of tokens created; copy-token creation
    /// (CR 707.5 / CR 707.2) is a token-creation event, so the same replacement
    /// applies: the doubling is applied first, then each copy enters with its
    /// own ETB. Issue #1511 regression: `CopyTokenOf` previously created exactly
    /// `count` copies, bypassing the `ProposedEvent::CreateToken` replacement
    /// pipeline, so the doubler never saw the copy.
    #[test]
    fn copy_token_count_doubling_replacement_applies() {
        let mut state = GameState::new_two_player(42);

        // Doubling-Season-style mandatory token-count doubler, controller-scoped.
        let doubler_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        {
            let doubler = state.objects.get_mut(&doubler_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::DOUBLE);
            doubler.base_replacement_definitions = Arc::new(vec![def.clone()]);
            doubler.replacement_definitions = vec![def].into();
        }

        // The copy source — a 3/1 Snake.
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mist-Syndicate Naga".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(1);
            source.power = Some(3);
            source.toughness = Some(1);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Snake".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 614.1a: count 1 doubled to 2 — two independent copy tokens.
        let copies: Vec<_> = state
            .objects
            .values()
            .filter(|o| o.is_token && o.name == "Mist-Syndicate Naga")
            .collect();
        assert_eq!(
            copies.len(),
            2,
            "token-count doubler must double a copy-token's count (issue #1511)"
        );
        // Each doubled copy enters with its own faithful characteristics + ETB.
        assert!(copies
            .iter()
            .all(|t| t.power == Some(3) && t.toughness == Some(1)));
        assert!(copies.iter().all(|t| t.zone == Zone::Battlefield));
        assert_eq!(
            state.last_created_token_ids.len(),
            2,
            "both doubled copy-token ids are recorded for downstream anaphora"
        );
        // CR 603.6a: each copy emits its own TokenCreated/ETB event.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(
                    e,
                    GameEvent::TokenCreated { name, .. } if name == "Mist-Syndicate Naga"
                ))
                .count(),
            2,
            "each doubled copy emits its own ETB/TokenCreated event"
        );
    }

    /// CR 616.1 + CR 707.2: If copy-token creation is modified by
    /// order-material token-count replacements, the resolver must pause for the
    /// affected player's choice and then resume by creating real copy tokens,
    /// not generic probe tokens.
    #[test]
    fn copy_token_replacement_choice_resumes_with_copy_payload() {
        let mut state = GameState::new_two_player(42);

        let doubler_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        {
            let doubler = state.objects.get_mut(&doubler_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::DOUBLE);
            doubler.base_replacement_definitions = Arc::new(vec![def.clone()]);
            doubler.replacement_definitions = vec![def].into();
        }

        let plus_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Token Augmenter".to_string(),
            Zone::Battlefield,
        );
        {
            let plus = state.objects.get_mut(&plus_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::Plus { value: 1 });
            plus.base_replacement_definitions = Arc::new(vec![def.clone()]);
            plus.replacement_definitions = vec![def].into();
        }

        let source_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Glasspool Mimic".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.printed_ref = Some(PrintedCardRef {
                oracle_id: "glasspool-oracle".to_string(),
                face_name: "Glasspool Mimic".to_string(),
            });
            source.base_printed_ref = source.printed_ref.clone();
            source.display_source = DisplaySource::Card;
            source.base_power = Some(3);
            source.base_toughness = Some(3);
            source.power = Some(3);
            source.toughness = Some(3);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Shapeshifter".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    candidate_count: 2,
                    ..
                }
            ),
            "non-commuting copy-token count replacements must prompt for CR 616 order"
        );
        assert!(
            state.last_created_token_ids.is_empty(),
            "no copy token should be created before the replacement choice resolves"
        );
        assert!(
            state.active_copy_token().is_some(),
            "the replacement prompt must be owned by the active CopyToken frame"
        );

        let wire = ResolutionStateWire::from_game_state(state);
        let serialized = serde_json::to_value(&wire).expect("CopyToken prompt serializes as v2");
        assert!(
            serialized.get("pending_copy_token_resolution").is_none(),
            "v2 CopyToken prompts must not emit the removed v1 field"
        );
        let mut state = serde_json::from_value::<ResolutionStateWire>(serialized)
            .expect("v2 CopyToken prompt roundtrips")
            .into_game_state();
        assert!(
            state.active_copy_token().is_some(),
            "roundtripped prompt keeps CopyToken as the active typed owner"
        );

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "copy-token resolution should finish after the replacement choice"
        );
        assert!(
            (3..=4).contains(&state.last_created_token_ids.len()),
            "chosen Double/Plus ordering should create three or four copies, not the unmodified one"
        );
        for token_id in &state.last_created_token_ids {
            let token = state.objects.get(token_id).unwrap();
            assert!(token.is_token);
            assert_eq!(token.name, "Glasspool Mimic");
            assert_eq!(token.power, Some(3));
            assert_eq!(token.toughness, Some(3));
            assert_eq!(token.display_source, DisplaySource::Card);
            assert_eq!(
                token
                    .printed_ref
                    .as_ref()
                    .map(|printed| printed.face_name.as_str()),
                Some("Glasspool Mimic"),
                "replacement-choice resume must use the copy payload, not generic TokenSpec apply"
            );
        }
    }

    /// CR 614.1c + CR 707.2: ETB-counter replacement mutations live on the
    /// accepted `CreateToken` event's `TokenSpec`; copy-token apply must consume
    /// them in addition to the CR 707 copy payload.
    #[test]
    fn copy_token_creation_applies_etb_counter_replacement_payload() {
        let mut state = GameState::new_two_player(42);

        let counter_replacement_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Mentor".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&counter_replacement_id).unwrap();
            let def = ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ))
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        target: TargetFilter::SelfRef,
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                ));
            source.base_replacement_definitions = Arc::new(vec![def.clone()]);
            source.replacement_definitions = vec![def].into();
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Runeclaw Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = state.last_created_token_ids[0];
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.name, "Runeclaw Bear");
        assert_eq!(
            token.counters.get(&CounterType::Plus1Plus1).copied(),
            Some(1),
            "copy-token apply must consume accepted TokenSpec enter_with_counters"
        );
    }

    /// Non-regression: without any token-count replacement active,
    /// `CopyTokenOf { count: N }` creates exactly N copies.
    #[test]
    fn copy_token_count_without_doubler_is_exact() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 3 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let copies = state
            .objects
            .values()
            .filter(|o| o.is_token && o.name == "Bear")
            .count();
        assert_eq!(copies, 3, "no doubler: exactly the requested count");
    }

    #[test]
    fn copy_token_propagates_printed_ref_for_image_lookup() {
        // A copy of a real-card permanent must carry the source's Scryfall
        // image hint (oracle_id + displayed face_name) so the frontend resolves
        // the same art. Regression: copying an MDFC face (The Prismatic Bridge)
        // produced a token with `printed_ref: None`, which rendered blank in the
        // legend-rule chooser because the back-face name is absent from the
        // front-face-only image index.
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "The Prismatic Bridge".to_string(),
            Zone::Battlefield,
        );
        let source_ref = crate::types::card::PrintedCardRef {
            oracle_id: "92023a5d-a143-4950-a71b-d736e6b8e959".to_string(),
            face_name: "The Prismatic Bridge".to_string(),
        };
        state.objects.get_mut(&source_id).unwrap().printed_ref = Some(source_ref.clone());

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        assert!(state.objects[&token_id].is_token);
        assert_eq!(
            state.objects[&token_id].printed_ref,
            Some(source_ref.clone()),
            "token copy must carry the source's printed_ref for image lookup"
        );

        // The fix is only durable if the token also carries `base_printed_ref`:
        // the layer reset restores `printed_ref` from the baseline each pass, so
        // without it the next `evaluate_layers` would wipe the art back to None.
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&token_id].printed_ref,
            Some(source_ref),
            "token copy's printed_ref must survive a layer evaluation pass"
        );
    }

    #[test]
    fn copy_token_of_target_creates_copy() {
        let mut state = GameState::new_two_player(42);

        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.power = Some(2);
            target.toughness = Some(2);
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copier".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.name, "Grizzly Bears");
        assert_eq!(token.power, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert!(token.is_token);
    }

    /// Issue #2402: Hazel of the Rootbloom — copy a non-Squirrel token target
    /// whose live characteristics must be mirrored into `base_*` fields by the
    /// real token creation path before copy-token resolution reads copiable values.
    #[test]
    fn issue_2402_copy_token_of_token_target_creates_copy() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hazel".to_string(),
            Zone::Battlefield,
        );
        let create_food = ResolvedAbility::new(
            Effect::Token {
                name: "Food".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Food".to_string()],
                colors: vec![],
                keywords: vec![],
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
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::token::resolve(&mut state, &create_food, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);
        let food = state.last_created_token_ids[0];
        let food_token = state.objects.get(&food).unwrap();
        assert!(food_token.base_characteristics_initialized);
        assert_eq!(food_token.base_name, "Food");
        assert_eq!(
            food_token.base_card_types.core_types,
            vec![CoreType::Artifact]
        );
        assert_eq!(food_token.base_card_types.subtypes, vec!["Food"]);

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(food)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let copy_id = ObjectId(state.next_object_id - 1);
        let copy = state.objects.get(&copy_id).unwrap();
        assert!(copy.is_token);
        assert_eq!(copy.name, "Food");
        assert_eq!(copy.card_types.core_types, vec![CoreType::Artifact]);
        assert_eq!(copy.card_types.subtypes, vec!["Food"]);
    }

    /// CR 109.4 + CR 111.2: "target opponent creates a token that's a copy of
    /// it" — the copy token must enter under the chosen opponent's control,
    /// not the trigger controller's. Pins the new `owner` channel at the
    /// building-block level (issue #403 defect 1).
    #[test]
    fn copy_token_of_owner_creates_under_chosen_player() {
        let mut state = GameState::new_two_player(42);

        // The copy source — a permanent controlled by PlayerId(0).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wedding Ring".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                // Copy source stays Wedding Ring itself.
                target: TargetFilter::SelfRef,
                // Non-context-ref owner filter — resolved from `ability.targets`.
                owner: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                ),
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            // The chosen opponent target.
            vec![TargetRef::Player(PlayerId(1))],
            source_id,
            // Wedding Ring's trigger controller is PlayerId(0).
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        // The token is a copy of Wedding Ring (the source), not a player.
        assert_eq!(token.name, "Wedding Ring");
        assert!(token.is_token);
        // CR 109.4: the token is controlled (and owned) by the chosen opponent.
        assert_eq!(
            token.controller,
            PlayerId(1),
            "copy token must be controlled by the chosen opponent, not the trigger controller"
        );
        assert_eq!(token.owner, PlayerId(1));
    }

    /// CR 109.4: the default `owner` of `TargetFilter::Controller` keeps the
    /// copy under the resolving ability's controller (the common case —
    /// populate, "you create a token that's a copy of …").
    #[test]
    fn copy_token_of_default_owner_is_controller() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.controller, PlayerId(0));
    }

    /// CR 609.3 + CR 101.3: An unattached Springheart Nantuko resolves
    /// `CopyTokenOf { target: AttachedTo }` with no host — `AttachedTo`
    /// resolves empty. The effect must be a clean zero-token no-op (no token
    /// created, `Ok` not `Err`) so the chained Insect-token fallback can fire.
    #[test]
    fn copy_token_of_empty_host_is_clean_no_op() {
        let mut state = GameState::new_two_player(42);
        // Source object with no `attached_to` — `AttachedTo` resolves empty.
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Springheart Nantuko".to_string(),
            Zone::Battlefield,
        );
        let objects_before = state.objects.len();

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::AttachedTo,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("empty host must be a clean no-op");

        assert_eq!(
            state.objects.len(),
            objects_before,
            "no token may be created when the AttachedTo host is empty"
        );
        assert!(
            state.last_created_token_ids.is_empty(),
            "no token ids recorded for an empty-host no-op"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::EffectResolved { .. })),
            "EffectResolved must still be emitted so the chain proceeds"
        );
    }

    #[test]
    fn copy_token_of_cost_paid_object_creates_requested_copies() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Osgir, the Reconstructor".to_string(),
            Zone::Battlefield,
        );
        let artifact_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ichor Wellspring".to_string(),
            Zone::Exile,
        );
        {
            let artifact = state.objects.get_mut(&artifact_id).unwrap();
            artifact.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            artifact.card_types = artifact.base_card_types.clone();
        }

        let snapshot = {
            let artifact = state.objects.get(&artifact_id).unwrap();
            CostPaidObjectSnapshot {
                object_id: artifact_id,
                lki: artifact.snapshot_for_mana_spent(),
                incarnation: 0,
            }
        };
        let mut ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::CostPaidObject,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 2 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.set_cost_paid_object_recursive(snapshot);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let copies: Vec<_> = state
            .objects
            .values()
            .filter(|object| object.is_token && object.name == "Ichor Wellspring")
            .collect();
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|token| token.zone == Zone::Battlefield));
        assert!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::TokenCreated { name, .. } if name == "Ichor Wellspring"
                ))
                .count()
                >= 2
        );
    }

    /// CR 603.10a / Vaultborn Tyrant + Ochre Jelly class: LTB self-copy triggers
    /// fire after the source has moved to the graveyard. The parsed effect is
    /// `CopyTokenOf { target: ParentTarget }` with empty `ability.targets`; the
    /// resolver must copy the source object from the graveyard.
    #[test]
    fn copy_token_of_parent_target_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vaultborn Tyrant".to_string(),
            Zone::Graveyard,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(6);
            source.base_toughness = Some(6);
            source.power = Some(6);
            source.toughness = Some(6);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dinosaur".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert_eq!(token.name, "Vaultborn Tyrant");
        assert_eq!(token.power, Some(6));
        assert_eq!(token.toughness, Some(6));
        // Source remains in graveyard (we only copy it, we don't move it).
        assert_eq!(state.objects[&source_id].zone, Zone::Graveyard);
    }

    /// CR 603.7 + CR 707.2: "copy of that card" after an exile instruction
    /// must read the tracked set published by the prior zone change. Copy
    /// sources are zone-agnostic, so an exiled card is a valid source.
    #[test]
    fn copy_token_of_tracked_set_source_from_exile() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kheru Goldkeeper".to_string(),
            Zone::Exile,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(3);
            source.base_toughness = Some(3);
            source.power = Some(3);
            source.toughness = Some(3);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Zombie".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![source_id]);

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(1),
                },
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: true,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(token.tapped);
        assert_eq!(token.name, "Kheru Goldkeeper");
        assert_eq!(token.power, Some(3));
        assert_eq!(token.toughness, Some(3));
    }

    #[test]
    fn copy_token_enters_tapped_and_attacking() {
        let mut state = GameState::new_two_player(42);

        // Set up combat
        state.combat = Some(crate::game::combat::CombatState::default());

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: true,
                tapped: true,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // CR 508.4: Token enters tapped and attacking
        assert!(token.tapped);
        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.iter().any(|a| a.object_id == token_id));
    }

    /// CR 707.2 + CR 702.10 (Haste): Twinflame's "except it has haste" — copy
    /// tokens carry the source's keywords plus the granted extra keyword.
    #[test]
    fn copy_token_extra_keywords_grant_haste() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Haste],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(token.keywords.contains(&Keyword::Haste));
        assert!(token.base_keywords.contains(&Keyword::Haste));
    }

    /// CR 115.1d + CR 601.2c: Twinflame's "for each of them" — multi-target
    /// CopyTokenOf creates one copy per object in `ability.targets`, and all
    /// created token IDs are recorded in `state.last_created_token_ids` so the
    /// "those tokens" anaphor in the delayed exile trigger captures the full
    /// set.
    #[test]
    fn copy_token_multi_target_creates_one_per_target() {
        let mut state = GameState::new_two_player(42);
        let bear_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        let bear_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        for id in [bear_a, bear_b] {
            let s = state.objects.get_mut(&id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }
        let twinflame_src = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Twinflame".to_string(),
            Zone::Stack,
        );
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Haste],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(bear_a), TargetRef::Object(bear_b)],
            twinflame_src,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        // Two new tokens, both with haste.
        assert_eq!(state.last_created_token_ids.len(), 2);
        for token_id in &state.last_created_token_ids {
            let t = state.objects.get(token_id).unwrap();
            assert!(t.is_token);
            assert!(t.keywords.contains(&Keyword::Haste));
        }
        // Names follow each respective source.
        let names: Vec<&str> = state
            .last_created_token_ids
            .iter()
            .map(|id| state.objects[id].name.as_str())
            .collect();
        assert!(names.contains(&"Bear A"));
        assert!(names.contains(&"Bear B"));
    }

    #[test]
    fn copy_token_source_filter_copies_matching_tokens_not_source() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 5;
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ocelot Pride".to_string(),
            Zone::Battlefield,
        );
        let cat_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Cat".to_string(),
            Zone::Battlefield,
        );
        let old_cat_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Old Cat".to_string(),
            Zone::Battlefield,
        );
        let opponent_cat_id = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opponent Cat".to_string(),
            Zone::Battlefield,
        );
        for (id, turn) in [
            (cat_id, Some(5)),
            (old_cat_id, Some(4)),
            (opponent_cat_id, Some(5)),
        ] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.is_token = true;
            obj.entered_battlefield_turn = turn;
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Cat".to_string()],
            };
            obj.card_types = obj.base_card_types.clone();
        }
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Controller,
                source_filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::Token, FilterProp::EnteredThisTurn],
                })),
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.last_created_token_ids.len(), 1);
        let copied = state.objects.get(&state.last_created_token_ids[0]).unwrap();
        assert_eq!(copied.name, "Cat");
        assert!(copied.is_token);
    }

    #[test]
    fn copy_token_source_filter_copies_graveyard_quest_counter_creature_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Altaïr Ibn-La'Ahad".to_string(),
            Zone::Battlefield,
        );
        let quest_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Quest Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&quest_creature).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Assassin".to_string()],
            };
            obj.card_types = obj.base_card_types.clone();
            obj.counters
                .insert(CounterType::Generic("quest".to_string()), 1);
        }
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::None,
                owner: TargetFilter::Controller,
                source_filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature, TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Generic(
                                "quest".to_string(),
                            )),
                            comparator: crate::types::ability::Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                })),
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.last_created_token_ids.len(), 1);
        let copied = state.objects.get(&state.last_created_token_ids[0]).unwrap();
        assert_eq!(copied.name, "Quest Creature");
        assert!(copied.is_token);
    }

    #[test]
    fn copy_token_source_filter_copies_each_matching_graveyard_source() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        let stale_token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Stale Token".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&stale_token).unwrap().is_token = true;
        state.last_created_token_ids = vec![stale_token];

        let first_match = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "First Match".to_string(),
            Zone::Graveyard,
        );
        let second_match = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Second Match".to_string(),
            Zone::Graveyard,
        );
        let wrong_counter = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Wrong Counter".to_string(),
            Zone::Graveyard,
        );
        for (id, counter) in [
            (first_match, "quest"),
            (second_match, "quest"),
            (wrong_counter, "lore"),
        ] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            obj.counters
                .insert(CounterType::Generic(counter.to_string()), 1);
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::None,
                owner: TargetFilter::Controller,
                source_filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature, TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Generic(
                                "quest".to_string(),
                            )),
                            comparator: crate::types::ability::Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                })),
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.last_created_token_ids.len(), 2);
        assert!(!state.last_created_token_ids.contains(&stale_token));
        let names: Vec<&str> = state
            .last_created_token_ids
            .iter()
            .map(|id| state.objects[id].name.as_str())
            .collect();
        assert!(names.contains(&"First Match"));
        assert!(names.contains(&"Second Match"));
        assert!(!names.contains(&"Wrong Counter"));
    }

    #[test]
    fn copy_token_source_filter_ignores_wrong_zone_and_wrong_counter() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        let battlefield_quest = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Battlefield Quest".to_string(),
            Zone::Battlefield,
        );
        let graveyard_lore = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Graveyard Lore".to_string(),
            Zone::Graveyard,
        );
        for (id, counter) in [(battlefield_quest, "quest"), (graveyard_lore, "lore")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            obj.counters
                .insert(CounterType::Generic(counter.to_string()), 1);
        }
        let next_before = state.next_object_id;
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::None,
                owner: TargetFilter::Controller,
                source_filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature, TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Generic(
                                "quest".to_string(),
                            )),
                            comparator: crate::types::ability::Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                })),
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.next_object_id, next_before);
        assert!(state.last_created_token_ids.is_empty());
    }

    #[test]
    fn copy_token_zero_match_clears_stale_last_created_before_cleanup() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        let stale_token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Stale Token".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&stale_token).unwrap().is_token = true;
        state.last_created_token_ids = vec![stale_token];

        let mut events = Vec::new();
        let copy_ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::None,
                owner: TargetFilter::Controller,
                source_filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature, TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Generic(
                                "quest".to_string(),
                            )),
                            comparator: crate::types::ability::Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                })),
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &copy_ability, &mut events).unwrap();
        assert!(state.last_created_token_ids.is_empty());

        let cleanup = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::LastCreated,
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
            vec![],
            source_id,
            PlayerId(0),
        );
        crate::game::effects::change_zone::resolve(&mut state, &cleanup, &mut events).unwrap();
        assert_eq!(state.objects[&stale_token].zone, Zone::Battlefield);
    }

    /// CR 205.4 + CR 707.9b + CR 704.5j: Miirym, Sentinel Wyrm class —
    /// `additional_modifications: [RemoveSupertype(Legendary)]` strips the
    /// Legendary supertype from the synthesized token. The legend rule
    /// (CR 704.5j) only collapses legendary permanents, so two such tokens
    /// must coexist on the battlefield without state-based action collapse.
    #[test]
    fn copy_token_remove_supertype_strips_legendary_from_token() {
        let mut state = GameState::new_two_player(42);
        // Source is a legendary creature (e.g., a Dragon).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        // Synthesize Miirym's CopyTokenOf with the RemoveSupertype modification.
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        // Layered view: Legendary stripped.
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must not be Legendary; got {:?}",
            token.card_types.supertypes
        );
        // Base view: Legendary stripped from the copiable values too — the
        // exception is part of the copy effect's bake-in (CR 707.2), so future
        // copies-of-this-token also start without Legendary.
        assert!(
            !token
                .base_card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "token's base_card_types must not contain Legendary; got {:?}",
            token.base_card_types.supertypes
        );
    }

    /// CR 205.4 + CR 707.9d: Adagia, Windswept Bastion class —
    /// `additional_modifications: [AddSupertype(Legendary)]` grants Legendary
    /// to a token copy of a non-legendary permanent.
    #[test]
    fn copy_token_add_supertype_grants_legendary_to_nonlegendary_source() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(
            token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must be Legendary; got {:?}",
            token.card_types.supertypes
        );
    }

    /// CR 704.5j + CR 707.9b: Issue #685 regression. When token-copy strips
    /// the Legendary supertype via `additional_modifications`, the legend
    /// rule SBA must NOT prompt the controller to choose which copy to
    /// sacrifice — the token is no longer Legendary, so there is exactly one
    /// Legendary permanent with the shared name (the original). Both
    /// permanents must remain on the battlefield. This is the SBA-side
    /// counterpart to the parser-side fix for the contracted "it's not
    /// legendary" form (Delina, Wild Mage; Ratadrabik of Urborg; etc.).
    #[test]
    fn legend_rule_does_not_fire_when_copy_token_drops_legendary() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = state.last_created_token_ids[0];

        // Run state-based actions; the legend rule SBA must NOT fire because
        // the token is not Legendary.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        assert!(
            !matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ChooseLegend { .. }
            ),
            "legend rule must not present a choice when token is not legendary; \
             got waiting_for={:?}",
            state.waiting_for
        );
        assert_eq!(
            state.objects[&source_id].zone,
            Zone::Battlefield,
            "original legendary creature must remain on battlefield"
        );
        assert_eq!(
            state.objects[&token_id].zone,
            Zone::Battlefield,
            "non-legendary token-copy must remain on battlefield"
        );
    }

    /// CR 704.5j + CR 707.9b: A token-copy exception that renames the copy
    /// avoids the legend rule through the generic `SetName` modification path,
    /// without any SBA special-case for Mishra, Eminent One.
    #[test]
    fn legend_rule_does_not_fire_when_copy_token_is_renamed() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mishra, Eminent One".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            source.card_types = source.base_card_types.clone();
        }

        let oracle = "create a token that's a copy of target noncreature artifact you control, except its name is ~'s Warform and it's a 4/4 Construct artifact creature in addition to its other types";
        let mut ctx = crate::parser::oracle_ir::context::ParseContext {
            card_name: Some("Mishra, Eminent One".to_string()),
            ..Default::default()
        };
        let effect =
            crate::parser::oracle_effect::try_parse_token(&oracle.to_lowercase(), oracle, &mut ctx)
                .expect("Mishra token-copy text should parse");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = &effect
        else {
            panic!("expected parser-produced CopyTokenOf, got {effect:?}");
        };
        assert!(
            additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::SetName { name } if name == "Mishra's Warform"
            )),
            "runtime regression must be driven by the parser-produced Mishra's Warform rename; got {additional_modifications:?}"
        );

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            effect,
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = state.last_created_token_ids[0];
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseLegend { .. }),
            "legend rule must not present a choice when the copy token has a distinct name; \
             got waiting_for={:?}",
            state.waiting_for
        );
        assert_eq!(state.objects[&source_id].zone, Zone::Battlefield);
        let token = &state.objects[&token_id];
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(token.name, "Mishra's Warform");
        assert_eq!(token.base_name, "Mishra's Warform");
        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(4));
        assert!(
            token.card_types.core_types.contains(&CoreType::Artifact),
            "renamed copy token must be an artifact; got {:?}",
            token.card_types.core_types
        );
        assert!(
            token.card_types.core_types.contains(&CoreType::Creature),
            "renamed copy token must be a creature; got {:?}",
            token.card_types.core_types
        );
        assert!(
            token.card_types.subtypes.contains(&"Construct".to_string()),
            "renamed copy token must be a Construct; got {:?}",
            token.card_types.subtypes
        );
    }

    #[test]
    fn mishra_shaped_copy_token_surfaces_liminal_enter_as_copy_choice() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mishra, Eminent One".to_string(),
            Zone::Battlefield,
        );
        let mirror_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Cursed Mirror".to_string(),
            Zone::Battlefield,
        );
        let target_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Silver Myr".to_string(),
            Zone::Battlefield,
        );
        {
            let mirror = state.objects.get_mut(&mirror_id).unwrap();
            mirror.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            mirror.card_types = mirror.base_card_types.clone();
            let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::BecomeCopy {
                        target: TargetFilter::Typed(TypedFilter::creature()),
                        recipient: crate::types::ability::CopyRecipient::Source,
                        duration: None,
                        mana_value_limit: None,
                        additional_modifications: Vec::new(),
                    },
                ));
            mirror.replacement_definitions.push(replacement.clone());
            Arc::make_mut(&mut mirror.base_replacement_definitions).push(replacement);
        }
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact, CoreType::Creature],
                subtypes: vec!["Myr".to_string()],
            };
            target.card_types = target.base_card_types.clone();
            target.base_power = Some(1);
            target.power = Some(1);
            target.base_toughness = Some(1);
            target.toughness = Some(1);
        }

        let oracle = "create a token that's a copy of target noncreature artifact you control, except its name is ~'s Warform and it's a 4/4 Construct artifact creature in addition to its other types";
        let mut ctx = crate::parser::oracle_ir::context::ParseContext {
            card_name: Some("Mishra, Eminent One".to_string()),
            ..Default::default()
        };
        let effect =
            crate::parser::oracle_effect::try_parse_token(&oracle.to_lowercase(), oracle, &mut ctx)
                .expect("Mishra token-copy text should parse");

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            effect,
            vec![TargetRef::Object(mirror_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::ReplacementChoice { .. } = state.waiting_for.clone() else {
            panic!(
                "expected optional liminal enter-as-copy ReplacementChoice, got {:?}",
                state.waiting_for
            );
        };
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accepting optional enter-as-copy replacement should resolve");

        let WaitingFor::CopyTargetChoice {
            source_id: liminal_id,
            valid_targets,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected liminal CopyTargetChoice, got {:?}",
                state.waiting_for
            );
        };
        assert!(
            state.liminal_entries.contains_key(&liminal_id),
            "copy choice source must be the uncommitted liminal token"
        );
        assert!(
            !state.objects.contains_key(&liminal_id),
            "liminal token must not be committed before the copy target choice"
        );
        assert!(valid_targets.contains(&target_id));

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
        )
        .expect("copy target choice should resolve");

        let token = state
            .objects
            .get(&liminal_id)
            .expect("liminal token should commit after choice");
        assert_eq!(token.name, "Mishra's Warform");
        assert_eq!(token.base_name, "Mishra's Warform");
        assert_eq!(token.power, Some(4));
        assert_eq!(token.toughness, Some(4));
        assert!(token.card_types.core_types.contains(&CoreType::Artifact));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.contains(&"Construct".to_string()));
    }

    /// CR 122.1 + CR 614.1c: AddCounterOnEnter with matching `if_type` places
    /// the counter on the synthesized token. Spark Double's planeswalker copy
    /// branch is exercised at the BecomeCopy resolver site; this test pins
    /// the same primitive on the token-copy path.
    #[test]
    fn copy_token_add_counter_on_enter_unconditional() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Soldier".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_power = Some(2);
            s.base_toughness = Some(2);
            s.power = Some(2);
            s.toughness = Some(2);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: None,
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        let p1p1 = token
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 1,
            "token should have one +1/+1 counter; counters={:?}",
            token.counters
        );
    }

    #[test]
    fn paused_copy_token_add_counter_on_enter_preserves_remaining_batch() {
        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Choice".to_string(),
            Zone::Battlefield,
        );
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent);
            def.mode = crate::types::ability::ReplacementMode::Optional { decline: None };
            let obj = state.objects.get_mut(&replacement_source).unwrap();
            obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
            obj.replacement_definitions = vec![def].into();
        }

        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Soldier".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(2);
            source.base_toughness = Some(2);
            source.power = Some(2);
            source.toughness = Some(2);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Soldier".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: None,
                }],
            },
            vec![TargetRef::Object(source_id)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(crate::types::resolution::ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![FrameKind::CopyToken, FrameKind::CounterAdditions],
            "a counter-replacement pause must keep CopyToken below its active counter child"
        );
        assert!(
            state.active_copy_token().is_none(),
            "top-only access must not search through the active counter child"
        );
        let serialized = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("nested CopyToken counter prompt serializes as v2");
        let mut state = serde_json::from_value::<ResolutionStateWire>(serialized)
            .expect("nested CopyToken counter prompt roundtrips")
            .into_game_state();
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(crate::types::resolution::ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![FrameKind::CopyToken, FrameKind::CounterAdditions],
            "v2 must preserve the CopyToken parent beneath its counter child"
        );

        let mut choice_events = Vec::new();
        for _ in 0..2 {
            let result =
                apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
            choice_events.extend(result.events);
        }

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(
            state.last_created_token_ids.len(),
            2,
            "copy-token counter pauses must preserve the current token and remaining copies"
        );
        for token_id in &state.last_created_token_ids {
            let token = state.objects.get(token_id).unwrap();
            assert!(token.is_token);
            assert_eq!(token.name, "Soldier");
        }
        assert_eq!(
            choice_events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::CopyTokenOf,
                        source_id: ObjectId(100),
                        ..
                    }
                ))
                .count(),
            1,
            "the copy-token effect should resolve once after the paused batch finishes"
        );
    }

    /// CR 707.9f: Conditional `if_type` declines when the resolved object's
    /// core type doesn't match. Token-copy of a non-creature with
    /// `AddCounterOnEnter { if_type: Some(Creature) }` must NOT place the
    /// counter (mirrors Spark Double's "if it's a creature" branch on a
    /// planeswalker copy).
    #[test]
    fn copy_token_add_counter_on_enter_if_type_mismatch_skips() {
        let mut state = GameState::new_two_player(42);
        // Copy source: a planeswalker (no Creature core type).
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_loyalty = Some(3);
            s.loyalty = Some(3);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: Some(CoreType::Creature),
                }],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        let p1p1 = token
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            p1p1, 0,
            "if_type=Creature must skip on a Planeswalker copy; counters={:?}",
            token.counters
        );
    }

    /// CR 306.5b + CR 306.5c + CR 707.2: A token that's a copy of a planeswalker
    /// must enter with loyalty counters equal to the copied loyalty — the copy
    /// path builds the object directly, bypassing the ZoneChange ETB-counter
    /// seeding, so it must seed loyalty counters itself. Pre-fix the copy carried
    /// only the `loyalty` field with no counter, so once the layer system treats
    /// the counter map as the source of truth (CR 306.5c) the copy would read 0
    /// loyalty and die immediately to CR 704.5i.
    #[test]
    fn copy_token_of_planeswalker_seeds_loyalty_counters() {
        use crate::game::layers::evaluate_layers;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&source_id).unwrap();
            s.base_loyalty = Some(5);
            s.loyalty = Some(5);
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        assert_eq!(
            state.objects[&token_id]
                .counters
                .get(&CounterType::Loyalty)
                .copied(),
            Some(5),
            "planeswalker copy must seed loyalty counters equal to copied loyalty",
        );

        // The copy must survive a layer re-evaluation at its real loyalty, not
        // snap to 0 (it's still on the battlefield, not in the graveyard).
        evaluate_layers(&mut state);
        let token = &state.objects[&token_id];
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(
            token.loyalty,
            Some(5),
            "copied planeswalker loyalty must derive from its seeded counters",
        );
    }

    /// CR 707.9b + CR 306.5b/c: Jace, Mirror Mage's token-copy exception sets
    /// the copy's starting loyalty to 1. This is not an extra loyalty counter;
    /// it replaces the loyalty value used for intrinsic planeswalker counter
    /// seeding, so the token must enter with one loyalty counter even if the
    /// copied source has a different loyalty value.
    #[test]
    fn copy_token_starting_loyalty_exception_overrides_seeded_loyalty() {
        use crate::game::layers::evaluate_layers;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jace, Mirror Mage".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_loyalty = Some(5);
            source.loyalty = Some(5);
            source.base_card_types = CardType {
                supertypes: vec![crate::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Jace".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::RemoveSupertype {
                        supertype: crate::types::card_type::Supertype::Legendary,
                    },
                    ContinuousModification::SetStartingLoyalty { value: 1 },
                ],
            },
            vec![TargetRef::Object(source_id)],
            source_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = &state.objects[&token_id];
        assert_eq!(token.base_loyalty, Some(1));
        assert_eq!(token.printed_loyalty, Some(PrintedLoyalty::Fixed(1)));
        assert_eq!(token.base_printed_loyalty, Some(PrintedLoyalty::Fixed(1)));
        assert_eq!(
            token.counters.get(&CounterType::Loyalty).copied(),
            Some(1),
            "starting-loyalty exception must seed one loyalty counter"
        );
        assert!(!token
            .card_types
            .supertypes
            .contains(&crate::types::card_type::Supertype::Legendary));

        evaluate_layers(&mut state);
        let token = &state.objects[&token_id];
        assert_eq!(token.zone, Zone::Battlefield);
        assert_eq!(token.loyalty, Some(1));
    }

    /// Regression: Helm of the Host (DOM, MH3, BLC) — pin the already-shipped
    /// non-legendary token-copy behavior so a future refactor cannot silently
    /// drop the `RemoveSupertype { Legendary }` stamp.
    ///
    /// Helm of the Host's begin-combat trigger creates a token that's a copy
    /// of equipped creature, "except the token isn't legendary." When the
    /// equipped creature IS legendary, the synthesized token must not be
    /// legendary — both the layered view (`card_types.supertypes`) and the
    /// copiable-values view (`base_card_types.supertypes`) must be free of
    /// `Supertype::Legendary`. Otherwise the legend rule (CR 704.5j) would
    /// collapse the token alongside its source.
    ///
    /// This test exercises the resolver with Helm's full ability shape:
    /// `Effect::CopyTokenOf { target: Typed[Creature]+EquippedBy,
    /// additional_modifications: [RemoveSupertype(Legendary)] }`. The general
    /// resolver behavior is also pinned by
    /// `copy_token_remove_supertype_strips_legendary_from_token` (Miirym
    /// class); this test anchors the named card so the behavior cannot
    /// regress without an explicit failure pointing at Helm of the Host.
    ///
    /// CR 707.9b + CR 205.4 + CR 301.5a: copy modifications, supertype
    /// semantics, and the equipped-creature relationship.
    #[test]
    fn helm_of_the_host_token_copy_strips_legendary_from_equipped_creature() {
        let mut state = GameState::new_two_player(42);

        // Equipped creature: a legendary 7/7 Dragon (e.g., Bahamut).
        let equipped_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bahamut".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&equipped_id).unwrap();
            s.base_power = Some(7);
            s.base_toughness = Some(7);
            s.power = Some(7);
            s.toughness = Some(7);
            s.base_card_types = CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            s.card_types = s.base_card_types.clone();
        }

        // Helm of the Host: non-legendary Equipment artifact attached to the
        // equipped creature. The trigger source for the begin-combat trigger.
        let helm_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Helm of the Host".to_string(),
            Zone::Battlefield,
        );
        {
            let s = state.objects.get_mut(&helm_id).unwrap();
            s.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
            };
            s.card_types = s.base_card_types.clone();
            s.attached_to = Some(equipped_id.into());
        }

        // Resolve Helm's begin-combat trigger: CopyTokenOf with the exact
        // Helm AST shape (`target: Typed[Creature]+EquippedBy`,
        // `additional_modifications: [RemoveSupertype(Legendary)]`). After
        // trigger resolution the engine has bound `EquippedBy` to the
        // equipped creature, so the resolved ability carries
        // `targets: [Object(equipped_id)]`.
        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::EquippedBy],
                }),
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![ContinuousModification::RemoveSupertype {
                    supertype: Supertype::Legendary,
                }],
            },
            vec![TargetRef::Object(equipped_id)],
            helm_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // CR 707.2: token copies the equipped creature's name, P/T, and types.
        assert!(token.is_token);
        assert_eq!(token.name, "Bahamut");
        assert_eq!(token.power, Some(7));
        assert_eq!(token.toughness, Some(7));

        // CR 707.9b + CR 205.4: layered view has Legendary stripped.
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token must not be Legendary; got supertypes={:?}",
            token.card_types.supertypes
        );

        // CR 707.9b: copiable-values view also has Legendary stripped — the
        // exception is part of the copy effect's bake-in, so future copies
        // of this token also start without Legendary.
        assert!(
            !token
                .base_card_types
                .supertypes
                .contains(&Supertype::Legendary),
            "token's base_card_types must not contain Legendary; got {:?}",
            token.base_card_types.supertypes
        );

        // CR 704.5j: with the original legendary creature and the
        // non-legendary token-copy both on the battlefield, the legend rule
        // SBA must NOT fire — there is exactly one Legendary permanent named
        // "Bahamut" (the source); the token shares the name but is not
        // legendary, so it is not a candidate for collapse.
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);

        assert!(
            !matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ChooseLegend { .. }
            ),
            "legend rule must not present a choice when the token is not legendary; \
             got waiting_for={:?}",
            state.waiting_for
        );
        // Both permanents survive on the battlefield.
        assert_eq!(
            state.objects[&equipped_id].zone,
            Zone::Battlefield,
            "original legendary creature must remain on battlefield"
        );
        assert_eq!(
            state.objects[&token_id].zone,
            Zone::Battlefield,
            "non-legendary token-copy must remain on battlefield"
        );
    }

    /// CR 702.175a: Offspring creates a token that's a copy of the creature,
    /// except it's 1/1. `SetPower`/`SetToughness` in `additional_modifications`
    /// must override the copied base P/T at creation time.
    #[test]
    fn offspring_token_is_1_1_not_copy_pt() {
        let mut state = GameState::new_two_player(42);

        // Create a 3/2 creature (the "parent" with offspring).
        let parent_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coruscation Mage".to_string(),
            Zone::Battlefield,
        );
        {
            let parent = state.objects.get_mut(&parent_id).unwrap();
            parent.base_power = Some(3);
            parent.base_toughness = Some(2);
            parent.power = Some(3);
            parent.toughness = Some(2);
            parent.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Wizard".to_string()],
            };
            parent.card_types = parent.base_card_types.clone();
        }

        let mut events = Vec::new();

        // Simulate the offspring ETB trigger: CopyTokenOf with SetPower(1), SetToughness(1).
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::SetPower { value: 1 },
                    ContinuousModification::SetToughness { value: 1 },
                ],
            },
            vec![],
            parent_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Find the token (newest object).
        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();

        // Token must be 1/1, not 3/2.
        assert_eq!(
            token.base_power,
            Some(1),
            "offspring token base_power must be 1"
        );
        assert_eq!(
            token.base_toughness,
            Some(1),
            "offspring token base_toughness must be 1"
        );
        assert_eq!(token.power, Some(1), "offspring token power must be 1");
        assert_eq!(
            token.toughness,
            Some(1),
            "offspring token toughness must be 1"
        );
        // Name and types are still copied.
        assert_eq!(token.name, "Coruscation Mage");
        assert!(token.card_types.subtypes.contains(&"Wizard".to_string()));
        assert!(token.is_token);
        assert!(
            !token
                .keywords
                .iter()
                .any(|kw| matches!(kw, crate::types::keywords::Keyword::Offspring(_))),
            "offspring token must not retain the cast-only Offspring keyword"
        );
        assert!(
            !token.trigger_definitions.iter_all().any(|trig| matches!(
                trig.definition.condition,
                Some(TriggerCondition::AdditionalCostPaid { .. })
            )),
            "offspring token must not retain AdditionalCostPaid ETB triggers"
        );
    }

    /// CR 707.2 vs CR 601.2f + CR 603.4: copy-token finalization strips only
    /// cast-payment-gated triggers ("if its offspring cost was paid" / "if it
    /// was kicked"). Persistent spell-cast observers — Magecraft's
    /// `SpellCastOrCopy`, generic "whenever you cast a [type] spell"
    /// `SpellCast` — are copiable battlefield text and must survive on the
    /// token copy.
    #[test]
    fn copy_token_keeps_spell_cast_triggers_strips_payment_gated_triggers() {
        let mut state = GameState::new_two_player(42);

        let parent_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Archmage Emeritus".to_string(),
            Zone::Battlefield,
        );
        {
            let parent = state.objects.get_mut(&parent_id).unwrap();
            parent.base_power = Some(2);
            parent.base_toughness = Some(2);
            parent.power = Some(2);
            parent.toughness = Some(2);
            parent.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Wizard".to_string()],
            };
            parent.card_types = parent.base_card_types.clone();

            // Magecraft-style persistent battlefield trigger (CR 707.10 note:
            // observes casts/copies; it is not itself cast-time bookkeeping).
            let magecraft = TriggerDefinition::new(TriggerMode::SpellCastOrCopy).description(
                "Magecraft — Whenever you cast or copy an instant or sorcery spell, draw a card."
                    .to_string(),
            );
            // Offspring-style ETB trigger gated on a cast-time payment.
            let offspring_etb = TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::SelfRef)
                .condition(TriggerCondition::AdditionalCostPaid {
                    source: AdditionalCostPaymentSource::Any,
                    origin: None,
                    origin_ordinal: None,
                    variant: None,
                    kicker_cost: None,
                    min_count: 1,
                })
                .description("offspring etb".to_string());
            parent.base_trigger_definitions = Arc::new(vec![magecraft, offspring_etb]);
            parent.trigger_definitions = Arc::clone(&parent.base_trigger_definitions).into();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            parent_id,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert!(token.is_token);
        assert!(
            token
                .trigger_definitions
                .iter_all()
                .any(|trig| matches!(trig.definition.mode, TriggerMode::SpellCastOrCopy)),
            "token copy must keep the persistent Magecraft SpellCastOrCopy trigger (CR 707.2)"
        );
        assert!(
            token
                .base_trigger_definitions
                .iter()
                .any(|trig| matches!(trig.mode, TriggerMode::SpellCastOrCopy)),
            "copiable base triggers must also keep the SpellCastOrCopy trigger (CR 707.2)"
        );
        assert!(
            !token.trigger_definitions.iter_all().any(|trig| matches!(
                trig.definition.condition,
                Some(TriggerCondition::AdditionalCostPaid { .. })
            )),
            "token copy must strip cast-payment-gated triggers (CR 601.2f + CR 603.4)"
        );
    }

    /// CR 707.9b: dynamic copy exceptions are resolved after the copied values
    /// are stamped onto the token, then baked into the token's base P/T.
    #[test]
    fn copy_token_dynamic_pt_exception_uses_copied_values() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sawed Beast".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(5);
            source.base_toughness = Some(4);
            source.power = Some(5);
            source.toughness = Some(4);
            source.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Beast".to_string()],
            };
            source.card_types = source.base_card_types.clone();
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![
                    ContinuousModification::SetPowerDynamic {
                        value: QuantityExpr::DivideRounded {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: ObjectScope::Source,
                                },
                            }),
                            divisor: 2,
                            rounding: RoundingMode::Up,
                        },
                    },
                    ContinuousModification::SetToughnessDynamic {
                        value: QuantityExpr::DivideRounded {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::Toughness {
                                    scope: ObjectScope::Source,
                                },
                            }),
                            divisor: 2,
                            rounding: RoundingMode::Up,
                        },
                    },
                ],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        assert_eq!(token.base_power, Some(3));
        assert_eq!(token.power, Some(3));
        assert_eq!(token.base_toughness, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert_eq!(token.name, "Sawed Beast");
        assert!(token.is_token);
    }

    /// Count copy-tokens of `copied_name` controlled by `player` on the
    /// battlefield (CR 111.2 — the player who creates a token is its owner and
    /// the token enters under that player's control).
    fn copy_tokens_for(state: &GameState, player: PlayerId, copied_name: &str) -> usize {
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.is_token && obj.name == copied_name && obj.controller == player)
            .count()
    }

    /// CR 707.2 + CR 608.2c + CR 109.4 + CR 608.2h: Fractured Identity end-to-end
    /// in a 3-player game. Exile a permanent P1 controls, then EACH PLAYER OTHER
    /// THAN ITS CONTROLLER (P1) creates a token copy. P0 and P2 each get exactly
    /// one copy; P1 (the exiled permanent's controller) gets none. The exclusion
    /// anchor resolves through the ability-aware `players_for_filter` using the
    /// exiled object's preserved last-known controller.
    #[test]
    fn fractured_identity_three_player_excludes_exiled_controller() {
        use crate::game::scenario::GameScenario;
        use crate::types::phase::Phase;

        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        // The exile target: a creature P1 owns and controls.
        let creature = scenario.add_creature(p1, "Grizzly Bears", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                p0,
                "Fractured Identity",
                false,
                "Exile target nonland permanent. Each player other than its controller \
                 creates a token that's a copy of it.",
            )
            .id();
        let mut runner = scenario.build();

        let outcome = runner.cast(spell).target_object(creature).resolve();
        let state = outcome.state();

        assert_eq!(
            state.objects[&creature].zone,
            Zone::Exile,
            "the targeted permanent must be exiled"
        );
        assert_eq!(
            copy_tokens_for(state, p0, "Grizzly Bears"),
            1,
            "P0 (not the controller) must create one copy"
        );
        assert_eq!(
            copy_tokens_for(state, p2, "Grizzly Bears"),
            1,
            "P2 (not the controller) must create one copy"
        );
        assert_eq!(
            copy_tokens_for(state, p1, "Grizzly Bears"),
            0,
            "P1 (the exiled permanent's controller) must NOT create a copy"
        );
    }

    /// CR 109.4 + CR 608.2h: "its controller" should anchor on the exiled
    /// permanent's last-known battlefield CONTROLLER, not its owner. A creature
    /// P1 owns but P0 controls (Mind Control style) is exiled; "each player other
    /// than its controller" should exclude P0, so only P1 creates a copy.
    ///
    /// `parent_target_controller` now prefers the LKI snapshot (captured before
    /// `reset_for_battlefield_exit` reverts the controller to the owner) for any
    /// object that is no longer on the battlefield (CR 608.2h).
    #[test]
    fn fractured_identity_its_controller_excludes_controller_not_owner() {
        use crate::game::scenario::GameScenario;
        use crate::types::phase::Phase;

        let p0 = PlayerId(0);
        let p1 = PlayerId(1);

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // A creature P1 OWNS but P0 CONTROLS.
        let creature = scenario.add_creature(p1, "Grizzly Bears", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                p0,
                "Fractured Identity",
                false,
                "Exile target nonland permanent. Each player other than its controller \
                 creates a token that's a copy of it.",
            )
            .id();
        let mut runner = scenario.build();
        // Simulate a stolen-control effect: P0 controls the P1-owned creature.
        // `base_controller` is the layer-stable control anchor (layers reset
        // `controller` to `base_controller.unwrap_or(owner)`), so both must be set
        // for the control change to survive recomputation.
        {
            let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
            obj.base_controller = Some(p0);
            obj.controller = p0;
        }

        let outcome = runner.cast(spell).target_object(creature).resolve();
        let state = outcome.state();

        assert_eq!(state.objects[&creature].zone, Zone::Exile);
        assert_eq!(
            copy_tokens_for(state, p1, "Grizzly Bears"),
            1,
            "P1 (not the controller) must create one copy"
        );
        assert_eq!(
            copy_tokens_for(state, p0, "Grizzly Bears"),
            0,
            "P0 (the controller of the exiled permanent) must NOT create a copy"
        );
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) enum CreationPauseCase {
        Synchronous,
        Queued,
        Counter,
        ReloadQueued,
        ReloadCounter,
    }

    pub(crate) fn creation_wrapper_case(
        effect: Effect,
        pause: CreationPauseCase,
    ) -> (GameState, Vec<GameEvent>) {
        // Labelled typed caller/entry isolation. The public full-card consumers
        // are separately tested in jev_token_followup and Dalkovan integration.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 902);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Caller source".into(),
            Zone::Battlefield,
        );
        {
            let object = state.objects.get_mut(&source).unwrap();
            object.base_card_types.core_types = vec![CoreType::Creature];
            object.card_types = object.base_card_types.clone();
            object.base_power = Some(2);
            object.base_toughness = Some(3);
            object.power = Some(2);
            object.toughness = Some(3);
            object.is_token = matches!(effect, Effect::Populate);
        }
        if matches!(effect, Effect::CreateTokenCopyFromPool { .. }) {
            state.card_db = Some(
                Arc::new(
                    crate::database::card_db::CardDatabase::from_mtgjson(
                        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../../data/mtgjson/test_fixture.json"),
                    )
                    .expect("existing pinned pool fixture"),
                )
                .into(),
            );
        }
        let mut combat = crate::game::combat::CombatState::default();
        if matches!(effect, Effect::CopyTokenBlockingAttacker { .. }) {
            state.objects.get_mut(&source).unwrap().controller = PlayerId(1);
            combat
                .attackers
                .push(crate::game::combat::AttackerInfo::new(
                    source,
                    crate::game::combat::AttackTarget::Player(PlayerId(0)),
                    PlayerId(0),
                ));
            let second = create_object(
                &mut state,
                CardId(2),
                PlayerId(1),
                "Second attacker".into(),
                Zone::Battlefield,
            );
            {
                let object = state.objects.get_mut(&second).unwrap();
                object.base_card_types.core_types = vec![CoreType::Creature];
                object.card_types = object.base_card_types.clone();
                object.base_power = Some(4);
                object.base_toughness = Some(5);
                object.power = Some(4);
                object.toughness = Some(5);
            }
            combat
                .attackers
                .push(crate::game::combat::AttackerInfo::new(
                    second,
                    crate::game::combat::AttackTarget::Player(PlayerId(0)),
                    PlayerId(0),
                ));
        } else {
            combat
                .attackers
                .push(crate::game::combat::AttackerInfo::new(
                    source,
                    crate::game::combat::AttackTarget::Player(PlayerId(1)),
                    PlayerId(1),
                ));
        }
        state.combat = Some(combat);
        if matches!(effect, Effect::Encore) {
            state.battlefield.retain(|id| *id != source);
            state.objects.get_mut(&source).unwrap().zone = Zone::Exile;
            state.exile.push_back(source);
        }
        let queued = matches!(
            pause,
            CreationPauseCase::Queued | CreationPauseCase::ReloadQueued
        );
        let counter = matches!(
            pause,
            CreationPauseCase::Counter | CreationPauseCase::ReloadCounter
        );
        let install = |state: &mut GameState, name: &str, definition: ReplacementDefinition| {
            let id = create_object(
                state,
                CardId(8),
                PlayerId(0),
                name.into(),
                Zone::Battlefield,
            );
            let object = state.objects.get_mut(&id).unwrap();
            object.base_replacement_definitions = Arc::new(vec![definition.clone()]);
            object.replacement_definitions = vec![definition].into();
        };
        if queued {
            let chatter = crate::parser::oracle::parse_oracle_text("Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.","Chatterfang, Squirrel General",&["Forestwalk".into()],&["Creature".into()],&["Squirrel".into()]);
            install(
                &mut state,
                "Chatterfang, Squirrel General",
                chatter.replacements[0].clone(),
            );
            let jinnie = crate::parser::oracle::parse_oracle_text("If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.","Jinnie Fay, Jetmir's Second",&[],&["Creature".into()],&[]);
            // Engine isolation of a genuinely queued optional child, so only
            // the appended Squirrel is eligible for this choice.
            install(
                &mut state,
                "Queued Squirrel substitution",
                jinnie.replacements[0].clone().condition(
                    crate::types::ability::ReplacementCondition::TokenSubtypeMatches {
                        subtypes: vec!["Squirrel".into()],
                    },
                ),
            );
        }
        if counter {
            install(
                &mut state,
                "Entry counter",
                ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                    .valid_card(TargetFilter::Any)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
            install(
                &mut state,
                "Counter double",
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::DOUBLE),
            );
            install(
                &mut state,
                "Counter plus",
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Plus { value: 1 }),
            );
        }
        let life_before = state.players[0].life;
        let mut ability = ResolvedAbility::new(effect, vec![], source, PlayerId(0));
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        )));
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();
        if queued || counter {
            assert!(
                matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
                "actual child prompt: {:?}",
                state.waiting_for
            );
            assert_eq!(
                state.players[0].life, life_before,
                "outer tail cannot run before the child answers"
            );
            assert!(!state.resolution_stack.is_empty());
            if matches!(ability.effect, Effect::Investigate) {
                let action = (
                    PlayerId(0),
                    crate::types::events::PlayerActionKind::Investigate,
                );
                assert!(!state.player_actions_this_way.contains(&action));
                assert!(!state.player_actions_this_turn.contains(&action));
                assert!(!events.iter().any(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::Investigate,
                        ..
                    }
                )));
            }
            if matches!(
                pause,
                CreationPauseCase::ReloadQueued | CreationPauseCase::ReloadCounter
            ) {
                state = serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
                )
                .unwrap()
                .into_game_state();
                assert_eq!(state.players[0].life, life_before);
            }
        }
        let mut replacement_answers = 0;
        let mut branch_answers = 0;
        for _ in 0..60 {
            let action = match state.waiting_for {
                WaitingFor::ReplacementChoice { .. } => {
                    replacement_answers += 1;
                    GameAction::ChooseReplacement { index: 0 }
                }
                WaitingFor::ChooseOneOfBranch { .. } => {
                    branch_answers += 1;
                    GameAction::ChooseBranch { index: 1 }
                }
                WaitingFor::Priority { .. } => {
                    super::super::resume_resolution_frames(&mut state, &mut events);
                    if state.resolution_stack.is_empty() {
                        break;
                    } else {
                        continue;
                    }
                }
                ref other => panic!("unexpected caller fixture prompt: {other:?}"),
            };
            events.extend(apply_as_current(&mut state, action).unwrap().events);
        }
        assert!(
            state.resolution_stack.is_empty(),
            "caller and all children retire"
        );
        assert_eq!(
            state.players[0].life,
            life_before + 1,
            "outer continuation exactly once"
        );
        let births = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !births.is_empty(),
            "actual entry reaches the caller completion hook"
        );
        assert_eq!(
            state
                .last_created_token_ids
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            births.iter().copied().collect()
        );
        for id in &births {
            assert!(events.iter().any(|event| matches!(event,GameEvent::ZoneChanged { object_id,from:None,to:Zone::Battlefield,.. } if object_id==id)));
        }
        if queued {
            assert!(replacement_answers > 0 && branch_answers > 0);
            assert!(state.objects.values().any(
                |object| object.is_token && object.card_types.subtypes.contains(&"Dog".into())
            ));
        }
        if counter {
            assert!(replacement_answers > 0);
            assert!(state
                .objects
                .values()
                .filter(|object| births.contains(&object.id))
                .all(|object| object
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0)
                    >= 3));
        }
        (state, events)
    }
    pub(crate) fn creation_test_source(
        state: &mut GameState,
        name: &str,
        types: Vec<CoreType>,
        subtypes: Vec<String>,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(500),
            PlayerId(0),
            name.into(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.base_card_types.core_types = types;
        object.base_card_types.subtypes = subtypes;
        object.card_types = object.base_card_types.clone();
        object.base_power = Some(power);
        object.base_toughness = Some(toughness);
        object.power = Some(power);
        object.toughness = Some(toughness);
        id
    }
    pub(crate) fn install_test_replacement(
        state: &mut GameState,
        name: &str,
        definition: ReplacementDefinition,
    ) {
        let id = create_object(
            state,
            CardId(501),
            PlayerId(0),
            name.into(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.base_replacement_definitions = Arc::new(vec![definition.clone()]);
        object.replacement_definitions = vec![definition].into();
    }
    fn cleanup_tail(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: crate::types::ability::DelayedTriggerCondition::AtNextPhase {
                    phase: crate::types::phase::Phase::End,
                },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Sacrifice {
                        target: TargetFilter::LastCreated,
                        count: QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![],
            source,
            PlayerId(0),
        )
    }
    #[test]
    fn copy_probe_projects_immediate_exceptions_without_state_or_deferred_side_effects() {
        let mut state = GameState::new_two_player(991);
        let source = creation_test_source(
            &mut state,
            "Noncreature origin",
            vec![CoreType::Artifact],
            vec!["Relic".into()],
            7,
            8,
        );
        state.objects.get_mut(&source).unwrap().mana_cost =
            crate::types::mana::ManaCost::generic(3);
        state.objects.get_mut(&source).unwrap().base_mana_cost =
            crate::types::mana::ManaCost::generic(3);
        let modifications = vec![
            ContinuousModification::SetName {
                name: "Immediate Construct".into(),
            },
            ContinuousModification::AddType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::AddSubtype {
                subtype: "Construct".into(),
            },
            ContinuousModification::SetPower { value: 4 },
            ContinuousModification::SetToughness { value: 4 },
            ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Fixed { value: 9 },
            },
            ContinuousModification::AddCounterOnEnter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                if_type: None,
            },
        ];
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Flying],
                additional_modifications: modifications,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let before = serde_json::to_value(&state).unwrap();
        let work = prepare_copy_creation(&mut state, &ability).unwrap();
        let TokenCreationWork::Proposed(batch) = work.front().unwrap() else {
            panic!("copy proposal")
        };
        let copy = batch.copy.as_deref().unwrap();
        let body = super::super::token::copy_probe_object_for(copy, &state.all_creature_types);
        assert_eq!(body.effective_mana_value(), 3);
        assert!(!copy_token_uses_liminal_entry(copy, &[]));
        let mut immediate = copy.clone();
        immediate.additional_modifications.clear();
        assert!(copy_token_uses_liminal_entry(&immediate, &[]));
        assert!(!copy_token_uses_liminal_entry(
            &immediate,
            &[(CounterType::Plus1Plus1, 1)]
        ));
        assert_eq!(
            copy_token_entry_counters(&immediate, &[(CounterType::Plus1Plus1, 1)]),
            vec![(CounterType::Plus1Plus1, 1)]
        );
        immediate.values.loyalty = Some(4);
        immediate
            .additional_modifications
            .push(ContinuousModification::SetStartingLoyalty { value: 2 });
        assert!(!copy_token_uses_liminal_entry(&immediate, &[]));
        assert_eq!(
            copy_token_entry_counters(&immediate, &[]),
            vec![(CounterType::Loyalty, 2)]
        );
        immediate.values.loyalty = None;
        immediate.additional_modifications = vec![ContinuousModification::RemoveManaCost];
        assert!(copy_token_uses_liminal_entry(&immediate, &[]));
        assert_eq!(
            super::super::token::copy_probe_object_for(&immediate, &[]).effective_mana_value(),
            0
        );
        let parsed_counter = crate::parser::parse_oracle_text(
            "This creature enters with a +1/+1 counter on it.",
            "Synthetic counter copy",
            &[],
            &["Creature".into()],
            &[],
        );
        assert_eq!(parsed_counter.replacements.len(), 1);
        immediate.values.replacement_definitions = parsed_counter.replacements;
        assert!(!copy_token_uses_liminal_entry(&immediate, &[]));
        assert_eq!(
            copy_token_entry_counters(&immediate, &[]),
            vec![(CounterType::Plus1Plus1, 1)]
        );
        let probe = super::super::token::copy_probe_spec_for(copy, &state.all_creature_types);
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            before,
            "projection cannot allocate, enter, consume rng or apply counters"
        );
        assert_eq!(probe.characteristics.display_name, "Immediate Construct");
        assert!(probe
            .characteristics
            .core_types
            .contains(&CoreType::Creature));
        assert!(probe.characteristics.subtypes.contains(&"Construct".into()));
        assert!(probe.characteristics.keywords.contains(&Keyword::Flying));
        assert_eq!(
            (probe.characteristics.power, probe.characteristics.toughness),
            (Some(4), Some(4))
        );
        assert!(probe.enter_with_counters.is_empty());
        let mut events = Vec::new();
        let progress = begin_creation(
            &mut state,
            &ability,
            work,
            EffectKind::CopyTokenOf,
            TokenCreationCallerState::Plain,
            &mut events,
        );
        let TokenCreationProgress::Completed(result) = progress else {
            panic!("single deferred entry completes")
        };
        assert_eq!(result.object_incarnations.len(), 1);
        let token = &state.objects[&result.object_incarnations[0].object_id];
        assert_eq!(token.counters.get(&CounterType::Plus1Plus1), Some(&1));
        assert_eq!(token.base_power, Some(9));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn ensure_batches_inherit_only_instruction_entry_axis() {
        // Labelled ensure isolation; a modified creature Clue is deliberately
        // not attributed to a printed card. Full ordinary investigate is public.
        for copy_backed in [false, true] {
            for (tapped, override_primary) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                for counter in [false, true] {
                    let mut state = GameState::new_two_player(992);
                    state.phase = crate::types::phase::Phase::PreCombatMain;
                    let source = creation_test_source(
                        &mut state,
                        "Superseded creature Clue",
                        vec![CoreType::Artifact, CoreType::Creature],
                        vec!["Clue".into()],
                        7,
                        8,
                    );
                    let manufactor=crate::parser::oracle::parse_oracle_text("If you would create a Clue, Food, or Treasure token, instead create one of each.","Academy Manufactor",&[],&["Creature".into()],&[]);
                    install_test_replacement(
                        &mut state,
                        "Academy Manufactor",
                        manufactor.replacements[0].clone(),
                    );
                    if override_primary {
                        install_test_replacement(
                            &mut state,
                            "Primary-only mutable entry override",
                            ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                                .valid_card(TargetFilter::Named {
                                    name: "Clue".into(),
                                })
                                .execute(AbilityDefinition::new(
                                    AbilityKind::Spell,
                                    Effect::SetTapState {
                                        target: TargetFilter::SelfRef,
                                        scope: crate::types::ability::EffectScope::Single,
                                        state: if tapped {
                                            crate::types::ability::TapStateChange::Untap
                                        } else {
                                            crate::types::ability::TapStateChange::Tap
                                        },
                                    },
                                )),
                        );
                    }
                    if counter {
                        let optional=crate::parser::oracle::parse_oracle_text("If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.","Jinnie Fay, Jetmir's Second",&[],&["Creature".into()],&[]);
                        install_test_replacement(
                            &mut state,
                            "Queued Food optional",
                            optional.replacements[0].clone().condition(
                                crate::types::ability::ReplacementCondition::TokenSubtypeMatches {
                                    subtypes: vec!["Food".into()],
                                },
                            ),
                        );
                        install_test_replacement(
                            &mut state,
                            "Entry counters",
                            ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                                .valid_card(TargetFilter::Any)
                                .execute(AbilityDefinition::new(
                                    AbilityKind::Spell,
                                    Effect::PutCounter {
                                        counter_type: CounterType::Generic("charge".into()),
                                        count: QuantityExpr::Fixed { value: 1 },
                                        target: TargetFilter::SelfRef,
                                    },
                                )),
                        );
                        install_test_replacement(
                            &mut state,
                            "Double counters",
                            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                                .quantity_modification(QuantityModification::DOUBLE),
                        );
                        install_test_replacement(
                            &mut state,
                            "Plus counters",
                            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                                .quantity_modification(QuantityModification::Plus { value: 1 }),
                        );
                    }
                    let mut ability = ResolvedAbility::new(
                        if copy_backed {
                            Effect::CopyTokenOf {
                                target: TargetFilter::SelfRef,
                                owner: TargetFilter::Controller,
                                source_filter: None,
                                enters_attacking: true,
                                tapped,
                                count: QuantityExpr::Fixed { value: 1 },
                                extra_keywords: vec![Keyword::DoubleStrike],
                                additional_modifications: vec![ContinuousModification::SetName {
                                    name: "Superseded creature Clue".into(),
                                }],
                            }
                        } else {
                            Effect::Token {
                                name: "Superseded creature Clue".into(),
                                power: PtValue::Fixed(7),
                                toughness: PtValue::Fixed(8),
                                types: vec!["Artifact".into(), "Creature".into(), "Clue".into()],
                                colors: vec![ManaColor::Blue],
                                keywords: vec![Keyword::DoubleStrike],
                                tapped,
                                count: QuantityExpr::Fixed { value: 1 },
                                owner: TargetFilter::Controller,
                                attach_to: None,
                                enters_attacking: true,
                                supertypes: vec![],
                                static_abilities: vec![],
                                enter_with_counters: vec![],
                            }
                        },
                        vec![],
                        source,
                        PlayerId(0),
                    );
                    ability.sub_ability = Some(Box::new(cleanup_tail(source)));
                    let mut events = Vec::new();
                    super::super::resolve_ability_chain(&mut state, &ability, &mut events, 1)
                        .unwrap();
                    if counter {
                        assert!(matches!(
                            state.waiting_for,
                            WaitingFor::ReplacementChoice { .. }
                        ));
                        assert!(state.delayed_triggers.is_empty());
                    }
                    let mut pauses = 0;
                    let mut optional_pauses = 0;
                    for _ in 0..60 {
                        match state.waiting_for {
                            WaitingFor::ReplacementChoice { .. } => {
                                pauses += 1;
                                let choice =
                                    if state.pending_replacement.as_ref().unwrap().is_optional {
                                        optional_pauses += 1;
                                        1
                                    } else {
                                        0
                                    };
                                state = serde_json::from_value::<ResolutionStateWire>(
                                    serde_json::to_value(ResolutionStateWire::from_game_state(
                                        state,
                                    ))
                                    .unwrap(),
                                )
                                .unwrap()
                                .into_game_state();
                                events.extend(
                                    apply_as_current(
                                        &mut state,
                                        GameAction::ChooseReplacement { index: choice },
                                    )
                                    .unwrap()
                                    .events,
                                );
                            }
                            WaitingFor::Priority { .. } if state.resolution_stack.is_empty() => {
                                break
                            }
                            ref other => panic!("ensure pause: {other:?}"),
                        }
                    }
                    if counter {
                        assert!(pauses > 0);
                        assert!(
                            optional_pauses > 0,
                            "actual queued ensure child optional pause survives reload"
                        );
                    }
                    assert!(state.resolution_stack.is_empty());
                    let delayed = state
                        .delayed_triggers
                        .iter()
                        .find(|trigger| trigger.source_id == source)
                        .unwrap();
                    assert_eq!(delayed.ability.target_incarnations.len(), 3);
                    let births = delayed.ability.target_incarnations.clone();
                    assert_eq!(
                        events
                            .iter()
                            .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                            .count(),
                        3
                    );
                    for name in ["Clue", "Food", "Treasure"] {
                        let token = births
                            .iter()
                            .map(|member| &state.objects[&member.object_id])
                            .find(|token| token.name == name)
                            .unwrap();
                        assert_eq!(token.card_types.core_types, vec![CoreType::Artifact]);
                        assert!(token.color.is_empty());
                        assert_eq!((token.base_power, token.base_toughness), (None, None));
                        assert!(!token.base_keywords.contains(&Keyword::DoubleStrike));
                        assert!(!token.abilities.is_empty());
                        assert_eq!(
                            token.tapped,
                            if override_primary && name == "Clue" {
                                !tapped
                            } else {
                                tapped
                            }
                        );
                        assert!(state.combat.as_ref().is_none_or(|combat| combat
                            .attackers
                            .iter()
                            .all(|attacker| attacker.object_id != token.id)));
                        if counter {
                            assert!(
                                token
                                    .counters
                                    .get(&CounterType::Generic("charge".into()))
                                    .copied()
                                    .unwrap_or(0)
                                    >= 3
                            );
                        }
                    }
                    let mut runner = crate::game::scenario::GameRunner::from_state(state);
                    for _ in 0..160 {
                        if runner
                            .state()
                            .stack
                            .iter()
                            .any(|entry| entry.source_id == source)
                        {
                            break;
                        }
                        runner.act(GameAction::PassPriority).unwrap();
                    }
                    assert!(!runner.state().stack.is_empty());
                    runner.advance_until_stack_empty();
                    for member in births {
                        assert_ne!(
                            runner.state().objects[&member.object_id].zone,
                            Zone::Battlefield
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn current_creation_authority_rejects_forged_ids_mixed_queues_and_duplicate_admission() {
        let mut state = GameState::new_two_player(993);
        let source = creation_test_source(
            &mut state,
            "Original",
            vec![CoreType::Creature],
            vec![],
            2,
            3,
        );
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        let work = prepare_copy_creation(&mut state, &ability).unwrap();
        let id = install_creation(
            &mut state,
            &ability,
            work,
            EffectKind::CopyTokenOf,
            TokenCreationCallerState::Plain,
        );
        let member = ObjectIncarnationRef::from_object(&state.objects[&source]);
        state
            .resolution_stack
            .admit_token_creation(id, member)
            .unwrap();
        assert!(state
            .resolution_stack
            .admit_token_creation(id, member)
            .is_err());
        assert!(state
            .resolution_stack
            .admit_token_creation(TokenCreationFrameId(id.0 + 500), member)
            .is_err());
        assert_eq!(
            state
                .resolution_stack
                .token_creation(id)
                .unwrap()
                .created_incarnations,
            vec![member]
        );
        let good =
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap();
        assert!(serde_json::from_value::<ResolutionStateWire>(good).is_ok());
        let TokenCreationWork::Proposed(proposed) = state
            .resolution_stack
            .token_creation(id)
            .unwrap()
            .work
            .front()
            .unwrap()
        else {
            unreachable!()
        };
        let legacy = PendingCopyTokenBatch {
            owner: proposed.owner,
            count: 1,
            copy: proposed.copy.clone().unwrap(),
        };
        for fault in 0..7 {
            let mut bad = state.clone();
            let owner = bad.resolution_stack.token_creation_mut(id).unwrap();
            match fault {
                0 => owner.creation_id = Some(TokenCreationFrameId(0)),
                1 => owner.result_parent = Some(TokenCreationFrameId(id.0 + 99)),
                2 => owner.creator = None,
                3 => owner.segment_start = 2,
                4 => owner.replacement_queue_tail = Some(0),
                5 => owner.remaining.push_back(legacy.clone()),
                _ => owner.created_incarnations.push(member),
            };
            assert!(
                serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(bad)).unwrap()
                )
                .is_err(),
                "malformed current authority {fault}"
            );
        }
        let mut duplicated =
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap();
        let frames = duplicated["resolution_frames"]["frames"]
            .as_array_mut()
            .unwrap();
        frames.push(frames[0].clone());
        assert!(serde_json::from_value::<ResolutionStateWire>(duplicated).is_err());
        let mut empty = state.clone();
        let owner = empty.resolution_stack.token_creation_mut(id).unwrap();
        owner.work.clear();
        owner.created_incarnations.clear();
        let progress = drive_creation(&mut empty, id, &mut Vec::new());
        assert!(
            matches!(progress,TokenCreationProgress::Completed(result) if result.object_incarnations.is_empty())
        );
        assert!(empty.resolution_stack.is_empty());
    }
    pub(crate) fn install_creation_fixture_replacements(
        state: &mut GameState,
        pause: CreationPauseCase,
    ) {
        if matches!(
            pause,
            CreationPauseCase::Queued | CreationPauseCase::ReloadQueued
        ) {
            let chatter=crate::parser::oracle::parse_oracle_text("Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.","Chatterfang, Squirrel General",&["Forestwalk".into()],&["Creature".into()],&[]);
            install_test_replacement(state, "Chatterfang", chatter.replacements[0].clone());
            let optional=crate::parser::oracle::parse_oracle_text("If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.","Jinnie Fay, Jetmir's Second",&[],&["Creature".into()],&[]);
            install_test_replacement(
                state,
                "Queued optional Squirrel",
                optional.replacements[0].clone().condition(
                    crate::types::ability::ReplacementCondition::TokenSubtypeMatches {
                        subtypes: vec!["Squirrel".into()],
                    },
                ),
            );
        }
        if matches!(
            pause,
            CreationPauseCase::Counter | CreationPauseCase::ReloadCounter
        ) {
            install_test_replacement(
                state,
                "Entry counter",
                ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                    .valid_card(TargetFilter::Any)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
            install_test_replacement(
                state,
                "Double counters",
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::DOUBLE),
            );
            install_test_replacement(
                state,
                "Plus counters",
                ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Plus { value: 1 }),
            );
        }
    }
    #[test]
    fn explicit_zero_prevented_and_unhosted_aura_finish_present_empty_without_foreign_latest() {
        for mode in 0..4 {
            let mut state = GameState::new_two_player(1001);
            let source = creation_test_source(
                &mut state,
                "Creation source",
                if mode >= 2 {
                    vec![CoreType::Enchantment]
                } else {
                    vec![CoreType::Creature]
                },
                if mode >= 2 {
                    vec!["Aura".into()]
                } else {
                    vec![]
                },
                2,
                3,
            );
            if mode >= 2 {
                let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
                let object = state.objects.get_mut(&source).unwrap();
                object.keywords = vec![enchant.clone()];
                object.base_keywords = vec![enchant];
            }
            let foreign = creation_test_source(
                &mut state,
                "Foreign latest",
                vec![CoreType::Artifact],
                vec![],
                11,
                13,
            );
            state.last_created_token_ids = vec![foreign];
            if mode == 1 {
                install_test_replacement(
                    &mut state,
                    "Prevent creation isolation",
                    ReplacementDefinition::new(ReplacementEvent::CreateToken)
                        .quantity_modification(QuantityModification::Prevent),
                );
            }
            let host = if mode == 3 {
                Some(creation_test_source(
                    &mut state,
                    "Legal host",
                    vec![CoreType::Creature],
                    vec![],
                    4,
                    5,
                ))
            } else {
                None
            };
            let mut ability = ResolvedAbility::new(
                Effect::CopyTokenOf {
                    target: TargetFilter::SelfRef,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    tapped: false,
                    count: QuantityExpr::Fixed {
                        value: if mode == 0 { 0 } else { 1 },
                    },
                    extra_keywords: vec![],
                    additional_modifications: vec![],
                },
                vec![],
                source,
                PlayerId(0),
            );
            let mut tail = cleanup_tail(source);
            tail.sub_ability = Some(Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                source,
                PlayerId(0),
            )));
            ability.sub_ability = Some(Box::new(tail));
            let life = state.players[0].life;
            let mut events = Vec::new();
            super::super::resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();
            assert!(state.resolution_stack.is_empty());
            assert_eq!(state.players[0].life, life + 1);
            let installed = state
                .delayed_triggers
                .iter()
                .find(|trigger| trigger.source_id == source)
                .unwrap();
            assert_eq!(
                installed.ability.target_incarnations.len(),
                usize::from(mode == 3)
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                    .count(),
                usize::from(mode == 3)
            );
            assert!(!installed
                .ability
                .targets
                .contains(&TargetRef::Object(foreign)));
            if let Some(host) = host {
                let member = installed.ability.target_incarnations[0];
                assert_eq!(
                    state.objects[&member.object_id].attached_to,
                    Some(crate::game::game_object::AttachTarget::Object(host))
                );
            } else {
                assert!(matches!(
                    installed.ability.effect,
                    Effect::Sacrifice {
                        target: TargetFilter::None,
                        count: QuantityExpr::Fixed { value: 0 },
                        ..
                    }
                ));
            }
        }
    }

    #[test]
    fn current_aura_host_and_vanished_counter_entries_resume_exact_owner_after_current_load() {
        for aura in [false, true] {
            let mut state = GameState::new_two_player(1002);
            let source = creation_test_source(
                &mut state,
                "Copy entry",
                if aura {
                    vec![CoreType::Enchantment]
                } else {
                    vec![CoreType::Creature]
                },
                if aura { vec!["Aura".into()] } else { vec![] },
                2,
                3,
            );
            let mut chosen = None;
            if aura {
                let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
                let object = state.objects.get_mut(&source).unwrap();
                object.keywords = vec![enchant.clone()];
                object.base_keywords = vec![enchant];
                chosen = Some(creation_test_source(
                    &mut state,
                    "First legal host",
                    vec![CoreType::Creature],
                    vec![],
                    4,
                    5,
                ));
                creation_test_source(
                    &mut state,
                    "Second legal host",
                    vec![CoreType::Creature],
                    vec![],
                    6,
                    7,
                );
            } else {
                install_creation_fixture_replacements(&mut state, CreationPauseCase::Counter);
            }
            let mut ability = ResolvedAbility::new(
                Effect::CopyTokenOf {
                    target: TargetFilter::SelfRef,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    extra_keywords: vec![],
                    additional_modifications: vec![ContinuousModification::SetPowerDynamic {
                        value: QuantityExpr::Fixed { value: 2 },
                    }],
                },
                vec![],
                source,
                PlayerId(0),
            );
            ability.sub_ability = Some(Box::new(cleanup_tail(source)));
            let mut events = Vec::new();
            super::super::resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();
            assert!(state.delayed_triggers.is_empty());
            assert!(!state.resolution_stack.is_empty());
            if aura {
                assert!(matches!(
                    state.waiting_for,
                    WaitingFor::ReturnAsAuraTarget { .. }
                ));
            } else {
                assert!(matches!(
                    state.waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ));
                let entrant = state
                    .objects
                    .values()
                    .find(|object| object.is_token)
                    .unwrap()
                    .id;
                state.objects.remove(&entrant);
                state.battlefield.retain(|id| *id != entrant);
            }
            state = serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
            )
            .unwrap()
            .into_game_state();
            if let Some(host) = chosen {
                events.extend(
                    apply_as_current(
                        &mut state,
                        GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(host)),
                        },
                    )
                    .unwrap()
                    .events,
                );
            } else {
                for _ in 0..20 {
                    if state.resolution_stack.is_empty() {
                        break;
                    }
                    events.extend(
                        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                            .unwrap()
                            .events,
                    );
                }
            }
            assert!(state.resolution_stack.is_empty());
            let installed = state
                .delayed_triggers
                .iter()
                .find(|trigger| trigger.source_id == source)
                .unwrap();
            assert_eq!(
                installed.ability.target_incarnations.len(),
                usize::from(aura)
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                    .count(),
                usize::from(aura)
            );
        }
    }

    #[test]
    fn unrelated_gift_completion_does_not_retire_or_join_a_resident_creation() {
        let mut state = GameState::new_two_player(1003);
        let source = creation_test_source(
            &mut state,
            "Parent source",
            vec![CoreType::Creature],
            vec![],
            2,
            3,
        );
        let mut ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(cleanup_tail(source)));
        let work = prepare_copy_creation(&mut state, &ability).unwrap();
        let id = install_creation(
            &mut state,
            &ability,
            work,
            EffectKind::CopyTokenOf,
            TokenCreationCallerState::Plain,
        );
        let mut gift = ResolvedAbility::new(
            Effect::GiftDelivery {
                kind: crate::types::keywords::GiftKind::Treasure,
            },
            vec![],
            source,
            PlayerId(0),
        );
        gift.context.additional_cost_paid = true;
        gift.context.gift_recipient = Some(PlayerId(1));
        let mut events = Vec::new();
        super::super::gift_delivery::resolve(&mut state, &gift, &mut events).unwrap();
        let gift_id = events
            .iter()
            .find_map(|event| {
                if let GameEvent::TokenCreated { object_id, .. } = event {
                    Some(*object_id)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(state.resolution_stack.token_creation(id).is_some());
        assert!(state
            .resolution_stack
            .token_creation(id)
            .unwrap()
            .created_incarnations
            .is_empty());
        let progress = drive_creation(&mut state, id, &mut events);
        assert!(
            matches!(progress,TokenCreationProgress::Completed(result) if result.object_incarnations.len()==1&&!result.targets.contains(&TargetRef::Object(gift_id)))
        );
        let installed = &state.delayed_triggers[0];
        assert_eq!(installed.ability.target_incarnations.len(), 1);
        assert!(!installed
            .ability
            .targets
            .contains(&TargetRef::Object(gift_id)));
        assert_eq!(state.objects[&gift_id].controller, PlayerId(1));
    }
    #[test]
    fn current_liminal_copy_counter_emit_keeps_exact_creation_owner_through_reload() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mishra, Eminent One".to_string(),
            Zone::Battlefield,
        );
        let mirror_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Cursed Mirror".to_string(),
            Zone::Battlefield,
        );
        let target_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Silver Myr".to_string(),
            Zone::Battlefield,
        );
        {
            let mirror = state.objects.get_mut(&mirror_id).unwrap();
            mirror.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
            };
            mirror.card_types = mirror.base_card_types.clone();
            let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::BecomeCopy {
                        target: TargetFilter::Typed(TypedFilter::creature()),
                        recipient: crate::types::ability::CopyRecipient::Source,
                        duration: None,
                        mana_value_limit: None,
                        additional_modifications: Vec::new(),
                    },
                ));
            mirror.replacement_definitions.push(replacement.clone());
            Arc::make_mut(&mut mirror.base_replacement_definitions).push(replacement);
        }
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact, CoreType::Creature],
                subtypes: vec!["Myr".to_string()],
            };
            target.card_types = target.base_card_types.clone();
            target.base_power = Some(1);
            target.power = Some(1);
            target.base_toughness = Some(1);
            target.toughness = Some(1);
        }

        let oracle = "create a token that's a copy of target noncreature artifact you control, except its name is ~'s Warform and it's a 4/4 Construct artifact creature in addition to its other types";
        let mut ctx = crate::parser::oracle_ir::context::ParseContext {
            card_name: Some("Mishra, Eminent One".to_string()),
            ..Default::default()
        };
        let effect =
            crate::parser::oracle_effect::try_parse_token(&oracle.to_lowercase(), oracle, &mut ctx)
                .expect("Mishra token-copy text should parse");

        let mut events = Vec::new();
        let mut ability = ResolvedAbility::new(
            effect,
            vec![TargetRef::Object(mirror_id)],
            source_id,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(cleanup_tail(source_id)));
        resolve(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::ReplacementChoice { .. } = state.waiting_for.clone() else {
            panic!(
                "expected optional liminal enter-as-copy ReplacementChoice, got {:?}",
                state.waiting_for
            );
        };
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accepting optional enter-as-copy replacement should resolve");

        let WaitingFor::CopyTargetChoice {
            source_id: liminal_id,
            valid_targets,
            ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected liminal CopyTargetChoice, got {:?}",
                state.waiting_for
            );
        };
        assert!(
            state.liminal_entries.contains_key(&liminal_id),
            "copy choice source must be the uncommitted liminal token"
        );
        assert!(
            !state.objects.contains_key(&liminal_id),
            "liminal token must not be committed before the copy target choice"
        );
        assert!(valid_targets.contains(&target_id));
        install_creation_fixture_replacements(&mut state, CreationPauseCase::Counter);
        state = serde_json::from_value::<ResolutionStateWire>(
            serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
        )
        .unwrap()
        .into_game_state();

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
        )
        .expect("copy target choice should resolve");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(state.delayed_triggers.is_empty());
        for _ in 0..30 {
            if state.resolution_stack.is_empty() {
                break;
            }
            state = serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
            )
            .unwrap()
            .into_game_state();
            events.extend(
                apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                    .unwrap()
                    .events,
            );
        }
        assert!(state.resolution_stack.is_empty());
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations.len(),
            1
        );
        assert_eq!(
            state.delayed_triggers[0].ability.target_incarnations[0].object_id,
            liminal_id
        );
        assert!(
            state.objects[&liminal_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
                >= 3
        );

        let token = state
            .objects
            .get(&liminal_id)
            .expect("liminal token should commit after choice");
        assert_eq!(token.name, "Mishra's Warform");
        assert_eq!(token.base_name, "Mishra's Warform");
        assert_eq!(token.base_power, Some(4));
        assert_eq!(token.base_toughness, Some(4));
        assert!(token.card_types.core_types.contains(&CoreType::Artifact));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.contains(&"Construct".to_string()));
    }

    #[test]
    fn divine_copy_body_substitution_survives_ordered_queued_approved_counter_reload() {
        let mut state = GameState::new_two_player(1004);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        let source = creation_test_source(
            &mut state,
            "Noncreature copy origin",
            vec![CoreType::Artifact],
            vec![],
            7,
            8,
        );
        let parsed=crate::parser::oracle::parse_oracle_text("If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.","Divine Visitation",&[],&["Enchantment".into()],&[]);
        install_test_replacement(
            &mut state,
            "Divine Visitation",
            parsed.replacements[0].clone(),
        );
        install_test_replacement(
            &mut state,
            "Double token count",
            ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .quantity_modification(QuantityModification::DOUBLE),
        );
        install_creation_fixture_replacements(&mut state, CreationPauseCase::Counter);
        let mut ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![Keyword::Indestructible],
                additional_modifications: vec![
                    ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    },
                    ContinuousModification::SetName {
                        name: "Superseded copied construct".into(),
                    },
                ],
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(cleanup_tail(source)));
        let mut work = prepare_copy_creation(&mut state, &ability).unwrap();
        work.push_back(work.front().unwrap().clone());
        let mut events = Vec::new();
        let pending = begin_creation(
            &mut state,
            &ability,
            work,
            EffectKind::CopyTokenOf,
            TokenCreationCallerState::Plain,
            &mut events,
        );
        assert!(matches!(pending, TokenCreationProgress::Pending(_)));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(state.delayed_triggers.is_empty());
        let mut prompts = 0;
        for _ in 0..100 {
            if state.resolution_stack.is_empty() {
                break;
            }
            assert!(matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            prompts += 1;
            state = serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
            )
            .unwrap()
            .into_game_state();
            events.extend(
                apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                    .unwrap()
                    .events,
            );
        }
        assert!(prompts >= 6);
        assert!(state.resolution_stack.is_empty());
        let births = state.delayed_triggers[0]
            .ability
            .target_incarnations
            .clone();
        assert_eq!(births.len(), 4);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                .count(),
            4
        );
        for member in &births {
            let object = &state.objects[&member.object_id];
            assert_eq!(
                (
                    object.name.as_str(),
                    object.base_power,
                    object.base_toughness
                ),
                ("Angel", Some(4), Some(4))
            );
            assert_eq!(object.card_types.core_types, vec![CoreType::Creature]);
            assert_eq!(object.color, vec![ManaColor::White]);
            assert!(object.keywords.contains(&Keyword::Flying));
            assert!(object.keywords.contains(&Keyword::Vigilance));
            assert!(!object.keywords.contains(&Keyword::Indestructible));
            assert!(
                object
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0)
                    >= 3
            );
        }
        let mut runner = crate::game::scenario::GameRunner::from_state(state);
        for _ in 0..150 {
            if !runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert!(!runner.state().stack.is_empty());
        runner.advance_until_stack_empty();
        for member in births {
            assert_ne!(
                runner.state().objects[&member.object_id].zone,
                Zone::Battlefield
            );
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    }
}

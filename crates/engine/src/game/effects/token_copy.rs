use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::game_object::{DisplayIdentity, DisplaySource, GameObject};
use crate::game::layers::{compute_current_copiable_values, remove_core_type, remove_subtype_set};
use crate::game::quantity::resolve_quantity;
use crate::game::{targeting, zones};
use crate::types::ability::{
    ChosenAttribute, ContinuousModification, CopiableValues, Effect, EffectError, EffectKind,
    EntryAttackDestination, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef,
    TriggerCondition, TriggerDefinition,
};
use crate::types::card::PrintedLoyalty;
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
    CopyTokenSpec, EtbTapState, ProposedEvent, TokenCharacteristics, TokenHostRequest,
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
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, declarations| resolve_owned(state, executable, events, declarations),
    )
}

pub(crate) fn resolve_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<(), EffectError> {
    begin_copy_creation_owned(
        state,
        ability,
        TokenCreationCallerState::Plain,
        events,
        declarations,
    )
    .map(|_| ())
}

pub(crate) fn begin_copy_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    caller: TokenCreationCallerState,
    events: &mut Vec<GameEvent>,
) -> Result<TokenCreationProgress, EffectError> {
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, declarations| {
            begin_copy_creation_owned(state, executable, caller, events, declarations)
        },
    )
}

pub(crate) fn begin_copy_creation_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    caller: TokenCreationCallerState,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<TokenCreationProgress, EffectError> {
    let work = prepare_copy_creation(state, ability)?;
    begin_creation_owned(
        state,
        ability,
        work,
        EffectKind::from(&ability.effect),
        caller,
        events,
        declarations,
    )
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
        attack_destination,
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
            attack_destination,
            tapped,
            count,
            extra_keywords,
            additional_modifications,
        } => (
            target,
            owner,
            source_filter,
            *enters_attacking,
            attack_destination,
            *tapped,
            count.clone(),
            extra_keywords.clone(),
            additional_modifications.clone(),
        ),
        _ => return Err(EffectError::MissingParam("CopyTokenOf".to_string())),
    };
    let (count, enters_attacking, attack_request) = super::token::resolve_token_batch(
        state,
        ability,
        &count_expr,
        enters_attacking,
        attack_destination,
    );

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
        } else if matches!(
            target_filter,
            TargetFilter::CostPaidObject {
                reference: crate::types::ability::CostPaidObjectRef::Source
            }
        ) {
            targeting::resolved_object_ids_for_filter(state, ability, target_filter)
        } else if matches!(
            target_filter,
            TargetFilter::CostPaidObject {
                reference: crate::types::ability::CostPaidObjectRef::Selected
            }
        ) {
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
            count,
            copy: Box::new(CopyTokenSpec {
                values: Box::new(source.values),
                // Created copy-tokens derive their descriptor from their
                // own base at injection (copy exceptions included), so the
                // source's captured body is deliberately not carried here.
                display: DisplayIdentity {
                    token_art: None,
                    ..source.display
                },
                extra_keywords: extra_keywords.clone(),
                additional_modifications: additional_modifications.clone(),
                tapped,
                enters_attacking,
                attack_request: attack_request.clone(),
                sacrifice_at: ability.duration.clone(),
                source_id: ability.source_id,
                controller: ability.controller,
                // CR 608.2h + CR 707.9b: carry the donor incarnation Step 1
                // bound so donor-referential exceptions halve donor values.
                donor: Some(source.incarnation),
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
    pub(crate) display: DisplayIdentity,
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
            display: source.display_identity(),
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
            display: DisplayIdentity {
                display_source: if token_image_ref.is_some() {
                    DisplaySource::Token
                } else {
                    DisplaySource::Card
                },
                printed_ref: None,
                token_image_ref,
                token_art: None,
            },
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
            attack_destination: crate::types::ability::TokenAttackDestination::ControllerChooses,
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
    .expect("fixed legacy copy batch has no instruction-result readers or producers")
}

pub(crate) fn copy_batches_to_work(
    state: &GameState,
    remaining: VecDeque<PendingCopyTokenBatch>,
) -> VecDeque<TokenCreationWork> {
    remaining
        .into_iter()
        .map(|batch| {
            let spec = super::token::copy_probe_spec_for(&batch.copy, &state.all_creature_types);
            let instructions = TokenCreationInstructions::from_legacy(
                EtbTapState::from_seeded_tapped(batch.copy.tapped),
                batch.copy.enters_attacking,
            );
            TokenCreationWork::Proposed(TokenCreationBatch {
                appended: Vec::new(),
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
) -> Result<TokenCreationProgress, EffectError> {
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, declarations| {
            begin_creation_owned(
                state,
                executable,
                work,
                effect_kind,
                caller,
                events,
                declarations,
            )
        },
    )
}

pub(crate) fn begin_creation_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    work: VecDeque<TokenCreationWork>,
    effect_kind: EffectKind,
    caller: TokenCreationCallerState,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<TokenCreationProgress, EffectError> {
    let id = install_creation_owned(state, ability, work, effect_kind, caller, declarations)?;
    if state
        .resolution_stack
        .token_creation(id)
        .expect("installed")
        .pending
        == Some(TokenCreationPending::CallerSelection)
    {
        return Ok(TokenCreationProgress::Pending(id));
    }
    Ok(drive_creation(state, id, events))
}

pub(crate) fn install_creation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    work: VecDeque<TokenCreationWork>,
    effect_kind: EffectKind,
    caller: TokenCreationCallerState,
) -> Result<TokenCreationFrameId, EffectError> {
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, declarations| {
            install_creation_owned(state, executable, work, effect_kind, caller, declarations)
        },
    )
}

pub(crate) fn install_creation_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    mut work: VecDeque<TokenCreationWork>,
    effect_kind: EffectKind,
    caller: TokenCreationCallerState,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<TokenCreationFrameId, EffectError> {
    super::validate_owned_instruction_result_cursor(
        state,
        ability,
        state.active_return_result_occurrence,
        declarations,
    )?;
    let mut creator = ability.clone();
    let continuation = match creator.sub_ability.take() {
        Some(mut tail) => {
            if super::should_propagate_parent_targets(ability, &tail) {
                tail.replace_target_occurrences(super::inherited_parent_occurrences(
                    ability, &tail,
                ));
            }
            super::project_parent_chain_context(
                &mut tail,
                ability,
                None,
                state.last_parent_target_missing_reason.clone(),
            );
            Some(Box::new(PendingContinuation::new_owned(
                tail,
                state,
                declarations,
            )?))
        }
        None => None,
    };
    if continuation.is_some() {
        state.last_parent_target_missing_reason = None;
    }
    let parent = creator.context.token_creation_parent.take();
    if let Some(parent) = parent.as_ref() {
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
            batch.spec.enters_attacking = parent.instructions.attack_destination.is_some();
            batch.spec.attack_request = parent.attack_request.clone();
            if let Some(copy) = batch.copy.as_mut() {
                copy.tapped = batch.spec.tapped;
                copy.enters_attacking = batch.spec.enters_attacking;
                copy.attack_request = parent.attack_request.clone();
            }
        }
    }
    for item in &mut work {
        let batch = match item {
            TokenCreationWork::Proposed(batch) | TokenCreationWork::Approved(batch) => batch,
        };
        let host = *batch
            .instructions
            .host_request
            .get_or_insert(batch.spec.attach_to);
        batch.spec.attach_to = host;
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
    Ok(id)
}

/// Store a settled primary behind the expansion work queued by its replacements.
pub(crate) fn approve_creation_batch(
    state: &mut GameState,
    event: ProposedEvent,
) -> TokenCreationFrameId {
    let ProposedEvent::CreateToken {
        appended,
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
                .as_ref()
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
            appended,
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
        let (mut batch, proposed) = match item {
            TokenCreationWork::Proposed(batch) => (batch, true),
            TokenCreationWork::Approved(batch) => (batch, false),
        };
        if batch.count == 0 && batch.appended.is_empty() {
            continue;
        }
        let host = *batch
            .instructions
            .host_request
            .get_or_insert(batch.spec.attach_to);
        batch.spec.attach_to = host;
        let event = ProposedEvent::CreateToken {
            appended: batch.appended,
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
            appended: Vec::new(),
            applied: state
                .post_replacement_token_choice_applied
                .clone()
                .unwrap_or_default(),
        };

        match crate::game::replacement::replace_event(state, proposed, events) {
            crate::game::replacement::ReplacementResult::Execute(event) => {
                // CR 614.1a + CR 608.2c: the frame is taken, so the delivery
                // authority leaves the ledger as this batch's whole created
                // referent, appended batches included: own batch first on
                // completion, `[]` after a batch paused with more of the event
                // still to deliver (the event publishes to this frame when it
                // finishes), and the ids so far after its last batch, a copy
                // batch, paused. `Suppress`: this frame's drain emits the
                // effect's terminal `EffectResolved{CopyTokenOf}`, so no pause
                // of the event owes one.
                let completed = super::token::apply_create_token_after_replacement_with_created_ids(
                    state,
                    event,
                    Vec::new(),
                    crate::types::game_state::PendingEffectResolutionEvent::Suppress,
                    events,
                );
                if !completed {
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
///
/// The single authority for re-parking a CopyToken owner around a child stack,
/// shared with the token-event delivery authority
/// (`token::drive_token_event_delivery`), which detaches the owner while it
/// delivers a compound CreateToken event.
pub(crate) fn park_copy_token_after_current_batch(
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
        token.base_display_source = Some(DisplaySource::Token);
        token.token_image_ref = Some(image_ref.clone());
        token.base_token_image_ref = Some(image_ref);
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
/// when its exceptions can be realized before entry. Counters use that entry's
/// replacement consultation and do not select a different materializer.
pub(crate) fn copy_token_uses_liminal_entry(copy: &CopyTokenSpec) -> bool {
    copy_token_modifications_are_liminal_immediate(&copy.additional_modifications)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_copy_token_after_replacement_with_created_ids(
    state: &mut GameState,
    creation_frame: Option<TokenCreationFrameId>,
    instructions: Option<TokenCreationInstructions>,
    token_owner: crate::types::player::PlayerId,
    copy: CopyTokenSpec,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(crate::types::counter::CounterType, u32)>,
    final_count: u32,
    initial_created_ids: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> CopyTokenApplyStatus {
    // CR 800.4b + CR 707.2: a token copy is a created token like any other; when
    // its settled controller has left the game none of this batch is created.
    // Returning before any per-token or liminal work keeps the created referent
    // to what an earlier pass of this batch already created; every caller
    // publishes `created_ids` into the ledger that owns the batch.
    if super::token::token_creator_has_left_game(state, token_owner) {
        return CopyTokenApplyStatus {
            created_ids: initial_created_ids,
            completion: CopyTokenApplyCompletion::Completed,
        };
    }
    let liminal_immediate = copy_token_uses_liminal_entry(&copy);
    // CR 306.5b + CR 614.12: only intrinsic loyalty lacks an explicit copied
    // replacement definition. Own entry definitions contribute on TokenEntry;
    // separate creating instructions stay on the entry and its batch remainder.
    let intrinsic_counters = crate::game::printed_cards::intrinsic_face_counters(
        copy_starting_loyalty_override(&copy.additional_modifications).or(copy.values.loyalty),
        None,
    );
    let etb_counters = if liminal_immediate {
        Vec::new()
    } else {
        copy_token_entry_counters(&copy, &enter_with_counters)
    };
    let CopyTokenSpec {
        mut values,
        display,
        extra_keywords,
        additional_modifications,
        tapped,
        enters_attacking,
        attack_request,
        sacrifice_at,
        source_id,
        controller,
        donor,
    } = copy;
    let name = values.name.clone();
    // CR 508.4: a resume carries the earlier ids so they are republished, but a
    // token's destination is selected by its ordinal among the tokens this call
    // creates, so the carried ids do not shift it.
    let prefix_len = initial_created_ids.len();
    let mut created_ids = initial_created_ids;
    created_ids.reserve(final_count as usize);
    if let Some(loyalty) = copy_starting_loyalty_override(&additional_modifications) {
        values.loyalty = Some(loyalty);
        values.printed_loyalty = Some(PrintedLoyalty::Fixed(loyalty));
    }

    // CR 306.5b + CR 614.12: immediate entries carry intrinsic loyalty on
    // TokenEntry, consult copied own definitions there, and retain only the
    // creating instruction's explicit counters on their per-entrant remainder.
    // The nonimmediate branch retains its already-merged counter work and
    // deferred modification/payment tails.
    // CR 205.3m: the live creature-type list the CR 707.9 subtype exceptions
    // resolve against. Loop-invariant, and cloned once so the per-token CR
    // 303.4f/g projection below can be built while `state` is borrowed.
    let all_creature_types = state.all_creature_types.clone();

    for index in 0..final_count {
        // CR 508.4: this token's own destination, selected by its creation
        // ordinal within the batch.
        let attack_destination = attack_request.for_entrant(created_ids.len() - prefix_len);
        if liminal_immediate {
            let (token_id, mut token) =
                super::token::reserve_liminal_token_object(state, token_owner, name.clone());
            let entry_timestamp = state.next_timestamp();

            let copy_spec = Box::new(CopyTokenSpec {
                values: values.clone(),
                display: display.clone(),
                extra_keywords: extra_keywords.clone(),
                additional_modifications: additional_modifications.clone(),
                tapped,
                enters_attacking,
                attack_request: attack_request.clone(),
                sacrifice_at: sacrifice_at.clone(),
                source_id,
                controller,
                donor,
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
            // CR 508.4: remove earlier actual births only. The commit seam
            // consumes this entrant's destination if it is actually created.
            let mut resume_copy = copy_spec.clone();
            resume_copy.attack_request =
                attack_request.after_entrants(created_ids.len() - prefix_len);
            state.liminal_entries.insert(
                token_id,
                LiminalEntry {
                    installed_copy_effects: Vec::new(),
                    copy_exception_counters: None,
                    creation_frame,
                    instructions,
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
                    attack_destination: attack_destination.clone(),
                    attach_to: instructions.and_then(|instruction| {
                        match instruction.host_request {
                            Some(crate::types::proposed_event::TokenHostRequest::Bound(host)) => {
                                Some(host)
                            }
                            _ => None,
                        }
                    }),
                    sacrifice_at: sacrifice_at.clone(),
                    remaining_count: final_count.saturating_sub(index + 1),
                    created_ids: created_ids.clone(),
                    copy_resume: Some(resume_copy),
                    spec_resume: None,
                    enter_tapped,
                    enter_with_counters: enter_with_counters.clone(),
                    kind: crate::types::game_state::LiminalEntryKind::Token,
                    replacement_applied: std::collections::HashSet::new(),
                },
            );

            let proposed = ProposedEvent::TokenEntry {
                entry_ref: token_id,
                enter_tapped,
                enter_with_counters: intrinsic_counters.clone(),
                enter_with_choices: None,
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
                    state
                        .abandon_liminal_token_entry(token_id)
                        .expect("prevented entry owns its selected copies");
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
            display: display.clone(),
            extra_keywords: extra_keywords.clone(),
            additional_modifications: additional_modifications.clone(),
            tapped,
            enters_attacking,
            attack_request: attack_request.clone(),
            sacrifice_at: sacrifice_at.clone(),
            source_id,
            controller,
            donor,
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

        // CR 508.4: the tail resumes the rest of the batch counting its own
        // tokens from zero, so its request starts past the tokens created so
        // far, this one included.
        let mut resume_copy = copy_spec.clone();
        resume_copy.attack_request =
            attack_request.after_entrants(created_ids.len() - prefix_len + 1);
        let tail = CopyTokenEntryTail {
            instructions,
            creation_frame,
            owner: token_owner,
            copy: resume_copy,
            enter_tapped,
            enter_with_counters: enter_with_counters.clone(),
            etb_counters: etb_counters.clone(),
            remaining_count: final_count.saturating_sub(index + 1),
            attack_destination: attack_destination.clone(),
        };

        // CR 614.12a + CR 707.2: a copied as-enters life payment (Saw in Half
        // copying Minion of the Wastes / Nameless Race) is a choice that
        // precedes the entry it modifies, so surface it BEFORE the CR 733
        // birth is journaled — the birth record carries the answered amount
        // (`entry_choices`). `None` (non-payer, or determined-0 already
        // latched inline) continues untouched; `Some` means the funnel parked
        // its prompt, so park the whole remaining entry tail — consult,
        // birth, aura attach, copy exceptions, entry counters, entry events,
        // and the rest of the batch — behind the player's answer. The prompt
        // is the funnel's own (`PayAmountChoice` raised by the surface call);
        // nothing here hand-raises a second one.
        // CR 614.12 + CR 613.1f: realize continuous effects for this entrant
        // BEFORE discovering its copied payment — an ability-removing effect
        // (Humility) must suppress a payment the entrant must not have.
        // Mirrors `finish_copy_target_choice_entry` (`engine_replacement.rs:2927`).
        crate::game::layers::mark_layers_full(state);
        crate::game::layers::flush_layers(state);
        if crate::game::replacement::surface_copied_entry_payment(state, token_id, events).is_some()
        {
            state.last_created_token_ids = created_ids.clone();
            super::counters::stash_pending_counter_post_actions(
                state,
                EffectKind::CopyTokenOf,
                source_id,
                vec![
                    PendingCounterPostAction::ContinueNonLiminalCopyTokenAfterEntryPayment {
                        object_id: token_id,
                        tail: Box::new(tail),
                    },
                ],
            );
            return CopyTokenApplyStatus {
                created_ids,
                completion: CopyTokenApplyCompletion::Paused,
            };
        }

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
        let host_request = instructions
            .and_then(|instruction| instruction.host_request)
            .unwrap_or(TokenHostRequest::NotRequested);
        let hosts = match entrant_projection.as_ref() {
            Some(entrant) if matches!(host_request, TokenHostRequest::NotRequested) => {
                crate::game::zone_pipeline::entering_aura_hosts_projected(state, token_id, entrant)
            }
            Some(_) | None => crate::game::zone_pipeline::EnteringAuraHosts::NotApplicable,
        };
        if entrant_projection.as_ref().is_some_and(|entrant| {
            super::token::aura_token_entry_denied(
                state,
                token_id,
                host_request,
                super::attach::AttachmentAuthority::Projected(entrant),
            )
        }) || matches!(
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
            // CR 614.12a + CR 707.2: the prompt path paused before this record,
            // so any `Number` latched on the entrant now is a determined-0 the
            // surface call committed inline (non-payers latch nothing). The
            // record carries exactly what the entrant answered — the resume's
            // own record point fills the same slot with the prompted answer.
            let entry_choices: Vec<ChosenAttribute> = state
                .objects
                .get(&token_id)
                .map(|token| {
                    token
                        .chosen_attributes
                        .iter()
                        .filter(|attr| matches!(attr, ChosenAttribute::Number(_)))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
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
                entry_choices,
                resulting_next_object_id: state.next_object_id,
                cause,
            };
            state
                .resolved_rules_journal
                .record_token_creation(command)
                .expect("resolved copy-token creation must have a live journal cause");
        }

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
    instructions: Option<TokenCreationInstructions>,
    creation_frame: Option<TokenCreationFrameId>,
    name: String,
    enters_attacking: bool,
    attack_destination: EntryAttackDestination,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
    /// CR 608.2h + CR 707.9b: donor incarnation donor-referential exceptions
    /// (`Source`-scoped dynamic P/T) read at the stamp; `None` reads the token.
    donor: Option<ObjectIncarnationRef>,
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
    let attack_destination = tail.attack_destination.clone();
    let source_id = copy_spec.source_id;
    let controller = copy_spec.controller;
    let additional_modifications = copy_spec.additional_modifications.clone();
    let name = copy_spec.values.name.clone();

    let instructions = tail.instructions;
    let finalization = CopyTokenFinalization {
        instructions,
        creation_frame: tail.creation_frame,
        name: name.clone(),
        enters_attacking,
        attack_destination: attack_destination.clone(),
        source_id,
        controller,
        donor: copy_spec.donor,
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
                    instructions,
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
                            instructions,
                            creation_frame: tail.creation_frame,
                            object_id: token_id,
                            name: name.clone(),
                            enters_attacking,
                            attack_destination: attack_destination.clone(),
                            source_id,
                            controller,
                        },
                        PendingCounterPostAction::ContinueCopyTokenCreation {
                            instructions,
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
    if !super::token::apply_entry_attack_instruction(
        state,
        token_id,
        instructions,
        enters_attacking,
        &attack_destination,
    ) {
        state.last_created_token_ids = sink.snapshot(state);
        super::counters::stash_pending_counter_post_actions(
            state,
            EffectKind::CopyTokenOf,
            source_id,
            vec![
                PendingCounterPostAction::FinalizeCopyTokenEntry {
                    instructions: super::token::settled_attack_instruction(instructions),
                    creation_frame: tail.creation_frame,
                    object_id: token_id,
                    name,
                    enters_attacking: false,
                    attack_destination,
                    source_id,
                    controller,
                },
                PendingCounterPostAction::ContinueCopyTokenCreation {
                    instructions,
                    creation_frame: tail.creation_frame,
                    owner: token_owner,
                    copy: copy_spec.clone(),
                    enter_tapped,
                    enter_with_counters: enter_with_counters.clone(),
                    remaining_count,
                },
            ],
        );
        return false;
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
    super::token::consume_token_host(
        state,
        token_id,
        instructions
            .and_then(|instruction| instruction.host_request)
            .unwrap_or(TokenHostRequest::NotRequested),
    );
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
    // goes through the shared resume authority, `resume_copy_token_batch_remainder`,
    // whose doc says where the continuation's ids land and why THIS token, already
    // published by `finish_non_liminal_copy_token_entry`'s `ResumedBatch` sink,
    // stays in the referent.
    resume_copy_token_batch_remainder(
        state,
        tail.instructions,
        tail.creation_frame,
        tail.owner,
        *tail.copy,
        tail.enter_tapped,
        tail.enter_with_counters,
        tail.remaining_count,
        events,
    )
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

/// CR 614.12a + CR 707.2: resume a non-liminal copy-token entry that paused on
/// its copied as-enters life payment. The funnel's completion tail already
/// latched the answered amount onto the live entrant; everything the loop
/// would have run after the surface call — consult, birth record (carrying
/// the answer), aura attach, copy exceptions, entry counters, entry events —
/// plus the rest of the batch runs here. The consult/record/aura/finish
/// sequence mirrors the loop body exactly (same helpers, same order); only
/// the birth's `entry_choices` differs (the prompted answer instead of a
/// determined-0/non-payer read).
pub(crate) fn continue_copy_token_after_entry_payment(
    state: &mut GameState,
    token_id: ObjectId,
    tail: CopyTokenEntryTail,
    events: &mut Vec<GameEvent>,
) -> bool {
    // The funnel latched exactly one `Number` answer onto the entrant (its
    // completion tail extends `chosen_attributes` with the parked amount). A
    // missing entrant — or one with no latch — voids this entry (nothing
    // moves the paused entrant during its own prompt, so this is defensive);
    // the batch still continues below.
    let answered = state.objects.get(&token_id).and_then(|token| {
        token.chosen_attributes.iter().find_map(|attr| match attr {
            ChosenAttribute::Number(amount) => Some(*amount),
            _ => None,
        })
    });
    if let Some(answer) = answered {
        // Redo the loop's projection + consult against the realized
        // post-payment entrant.
        let all_creature_types = state.all_creature_types.clone();
        let entrant_projection = state.objects.get(&token_id).map(|token| {
            let mut projection = token.clone();
            apply_immediate_copy_token_modifications_to_object(
                &mut projection,
                &tail.copy.additional_modifications,
                &all_creature_types,
            );
            projection
        });
        let host_request = tail
            .instructions
            .and_then(|instruction| instruction.host_request)
            .unwrap_or(TokenHostRequest::NotRequested);
        let hosts = match entrant_projection.as_ref() {
            Some(entrant) if matches!(host_request, TokenHostRequest::NotRequested) => {
                crate::game::zone_pipeline::entering_aura_hosts_projected(state, token_id, entrant)
            }
            Some(_) | None => crate::game::zone_pipeline::EnteringAuraHosts::NotApplicable,
        };
        if entrant_projection.as_ref().is_some_and(|entrant| {
            super::token::aura_token_entry_denied(
                state,
                token_id,
                host_request,
                super::attach::AttachmentAuthority::Projected(entrant),
            )
        }) || matches!(
            &hosts,
            crate::game::zone_pipeline::EnteringAuraHosts::Hosts { legal_targets, .. }
                if legal_targets.is_empty()
        ) {
            // CR 303.4g: same uncreate as the loop's gate — no birth was
            // recorded (the pause precedes the record), so withholding it
            // here is identical.
            super::token::uncreate_unentered_aura_token(state, token_id, tail.owner);
        } else {
            // CR 733: journal the birth with the answered amount. Timestamp
            // and turn are read off the entrant itself (written at
            // materialize, so they match the token exactly); the high-water
            // mark reads the live allocator (nothing allocates between the
            // pause and this resume — the interleaving payment moves life
            // only). `from_object` carries no attribute snapshot, so the
            // latched answer cannot double-install at replay.
            let turn_number = state.turn_number;
            let live = state.objects.get(&token_id).map(|token| {
                (
                    ObjectIncarnationRef::from_object(token),
                    token.timestamp,
                    token.entered_battlefield_turn.unwrap_or(turn_number),
                )
            });
            if let Some((object, entry_timestamp, entry_turn)) = live {
                let cause = state.current_or_begin_rules_execution_node();
                let body_modifications = if tail.copy.additional_modifications.is_empty() {
                    ResolvedCopyBodyModifications::NoExceptions
                } else {
                    ResolvedCopyBodyModifications::DeferredToUnjournaledSeam {
                        modifications: tail.copy.additional_modifications.clone(),
                    }
                };
                let command = ResolvedTokenCreationCommand {
                    object,
                    owner: tail.owner,
                    entry_timestamp,
                    entry_turn,
                    body: ResolvedTokenBody::Copy {
                        copy: tail.copy.clone(),
                        modifications: body_modifications,
                    },
                    resulting_tapped: tail.enter_tapped.resolve(tail.copy.tapped),
                    entry_choices: vec![ChosenAttribute::Number(answer)],
                    resulting_next_object_id: state.next_object_id,
                    cause,
                };
                state
                    .resolved_rules_journal
                    .record_token_creation(command)
                    .expect("resumed copy-token creation must have a live journal cause");
            }
            // Aura attach (mirror of the loop's apply block): a host choice
            // pauses again behind the AURA variant — the birth above is
            // already journaled, so that resume's straight-to-finish shape
            // stays sound.
            match crate::game::zone_pipeline::apply_entering_aura_hosts(state, token_id, hosts) {
                crate::game::zone_pipeline::EnteringAuraAttachment::NotApplicable
                | crate::game::zone_pipeline::EnteringAuraAttachment::Attached
                | crate::game::zone_pipeline::EnteringAuraAttachment::NoLegalHost => {}
                crate::game::zone_pipeline::EnteringAuraAttachment::NeedsChoice {
                    controller: chooser,
                    legal_targets,
                } => {
                    // No ledger snapshot (unlike the loop's arm): ledger 3
                    // already holds exactly the pre-this-token ids — the
                    // payment pause snapshotted them and nothing completed
                    // since.
                    let tail_source_id = tail.copy.source_id;
                    super::counters::stash_pending_counter_additions(
                        state,
                        Vec::new(),
                        crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                            EffectKind::CopyTokenOf,
                            tail_source_id,
                            vec![PendingCounterPostAction::ContinueCopyTokenEntryAfterAuraHost {
                                object_id: token_id,
                                tail: Box::new(tail),
                            }],
                        ),
                    );
                    state.waiting_for = WaitingFor::ReturnAsAuraTarget {
                        player: chooser,
                        source_id: tail_source_id,
                        returned_id: token_id,
                        legal_targets,
                        pending_effect: Box::new(crate::types::ability::ResolvedAbility::new(
                            crate::types::ability::Effect::Attach {
                                attachment: crate::types::ability::TargetFilter::SelfRef,
                                target: crate::types::ability::TargetFilter::Any,
                                selection: crate::types::ability::AttachSelection::AtResolution {
                                    count: crate::types::ability::AttachCardinality::One,
                                },
                            },
                            Vec::new(),
                            tail_source_id,
                            chooser,
                        )),
                    };
                    return false;
                }
            }
            if !finish_non_liminal_copy_token_entry(
                state,
                token_id,
                &tail,
                &mut CopyBatchIdSink::ResumedBatch(tail.creation_frame),
                events,
            ) {
                return false;
            }
        }
    }
    // CR 707.2: the paused token is done; drive the rest of the batch.
    // Publication goes through the shared resume authority,
    // `resume_copy_token_batch_remainder`.
    resume_copy_token_batch_remainder(
        state,
        tail.instructions,
        tail.creation_frame,
        tail.owner,
        *tail.copy,
        tail.enter_tapped,
        tail.enter_with_counters,
        tail.remaining_count,
        events,
    )
}

/// CR 707.2 + CR 608.2c: drive the rest of a paused copy batch and publish its ids
/// after the referent the creating event built so far. The single authority for
/// every post-action that restarts a paused batch (counter pause, Aura host,
/// copied entry payment).
///
/// While a CopyToken owner is active, its `created_ids` buffer is the referent
/// (assigned wholesale onto `last_created_token_ids` at its drain), so the
/// continuation's ids extend it. With none — the plain `Effect::Token` route, or a
/// compound token event whose cursor re-parked the owner below this pause — the
/// ledger is the destination, holding what the pause left there plus the token
/// that just finished (on the plain route, the event's referent so far; under the
/// compound cursor, possibly only the paused batch's ids, which the cursor
/// reconciles against the ids it already holds when it resumes). It is handed to
/// the continuation as its `initial_created_ids` prefix, so a pause inside the
/// continuation republishes it rather than a continuation-local list, and the
/// completed list replaces it.
///
/// The token whose own entry just finished is already in whichever destination
/// owns the batch (`token::record_last_created_copy_batch_token` published it,
/// from the counter pause's finalize post-action or through
/// `finish_non_liminal_copy_token_entry`'s `ResumedBatch` sink): extending the
/// owner's buffer keeps it, and with no owner it is part of the prefix the
/// completed list starts with.
pub(crate) fn resume_copy_token_batch_remainder(
    state: &mut GameState,
    instructions: Option<TokenCreationInstructions>,
    creation_frame: Option<TokenCreationFrameId>,
    owner: crate::types::player::PlayerId,
    copy: CopyTokenSpec,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(crate::types::counter::CounterType, u32)>,
    remaining_count: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    if remaining_count == 0 {
        return true;
    }
    let has_owner = match creation_frame {
        Some(id) => state.resolution_stack.token_creation(id).is_some(),
        None => state
            .active_copy_token()
            .is_some_and(|pending| pending.creation_id.is_none()),
    };
    let initial_created_ids = if has_owner {
        Vec::new()
    } else {
        std::mem::take(&mut state.last_created_token_ids)
    };
    let status = apply_copy_token_after_replacement_with_created_ids(
        state,
        creation_frame,
        instructions,
        owner,
        copy,
        enter_tapped,
        enter_with_counters,
        remaining_count,
        initial_created_ids,
        events,
    );
    if has_owner {
        extend_copy_batch_created_ids(state, creation_frame, status.created_ids);
    } else {
        state.last_created_token_ids = status.created_ids;
    }
    matches!(status.completion, CopyTokenApplyCompletion::Completed)
}

/// CR 707.2 / CR 707.9: Complete copy-token entry and apply remaining copy
/// modifications after resuming from a counter-placement replacement pause.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_remaining_token_modifications_after_counter_pause(
    state: &mut GameState,
    instructions: Option<TokenCreationInstructions>,
    creation_frame: Option<TokenCreationFrameId>,
    token_id: ObjectId,
    name: String,
    enters_attacking: bool,
    attack_destination: EntryAttackDestination,
    source_id: ObjectId,
    controller: crate::types::player::PlayerId,
    remaining_modifications: Vec<ContinuousModification>,
    donor: Option<ObjectIncarnationRef>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let finalization = CopyTokenFinalization {
        instructions,
        creation_frame,
        name: name.clone(),
        enters_attacking,
        attack_destination,
        source_id,
        controller,
        donor,
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
    if !super::token::apply_entry_attack_instruction(
        state,
        token_id,
        instructions,
        enters_attacking,
        &finalization.attack_destination,
    ) {
        super::counters::stash_pending_counter_post_actions(
            state,
            EffectKind::CopyTokenOf,
            source_id,
            vec![PendingCounterPostAction::FinalizeCopyTokenEntry {
                instructions: super::token::settled_attack_instruction(instructions),
                creation_frame,
                object_id: token_id,
                name,
                enters_attacking: false,
                attack_destination: finalization.attack_destination,
                source_id,
                controller,
            }],
        );
        return false;
    }
    super::token::inject_predefined_token_abilities(state, token_id);
    super::token::consume_token_host(
        state,
        token_id,
        instructions
            .and_then(|instruction| instruction.host_request)
            .unwrap_or(TokenHostRequest::NotRequested),
    );
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
/// `SetPowerDynamic` / `SetToughnessDynamic` (Saw in Half class) ARE stamped
/// below: their `Source`-scoped quantities read the donor via
/// `CopyTokenFinalization.donor` (live, else LKI per CR 608.2h) and land as
/// token base P/T (CR 707.9b).
///
/// Modifications not relevant to token-copy semantics (e.g. `CopyValues`,
/// `ChangeController`) are skipped silently — they have no meaningful "stamp
/// at creation" interpretation. A future card with such an except body will
/// surface as an unimplemented modification, which is strictly better than
/// silently mutating the token incorrectly.
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
                                instructions: finalization.instructions,
                                creation_frame: finalization.creation_frame,
                                object_id: token_id,
                                name: finalization.name.clone(),
                                enters_attacking: finalization.enters_attacking,
                                attack_destination: finalization.attack_destination.clone(),
                                source_id: finalization.source_id,
                                controller: finalization.controller,
                                remaining_modifications: modifications[index + 1..].to_vec(),
                                donor: finalization.donor,
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
                // CR 608.2h + CR 707.9b: `Source`-scoped donor phrases ("that
                // creature's power") read the donor (live, else LKI); donor-less
                // routes keep today's token-id read.
                let source_id = finalization.donor.map(|d| d.object_id).unwrap_or(token_id);
                let val = resolve_quantity(state, value, controller, source_id);
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
                // CR 608.2h + CR 707.9b: `Source`-scoped donor phrases ("that
                // creature's toughness") read the donor (live, else LKI); donor-less
                // routes keep today's token-id read.
                let source_id = finalization.donor.map(|d| d.object_id).unwrap_or(token_id);
                let val = resolve_quantity(state, value, controller, source_id);
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
        ControllerRef, CopyTargetPurpose, CostPaidObjectSnapshot, Duration, Effect, FilterProp,
        ObjectScope, PtValue, QuantityExpr, QuantityModification, QuantityRef,
        ReplacementDefinition, ReplacementMode, RoundingMode, TargetFilter, TargetRef, TypeFilter,
        TypedFilter,
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

    /// CR 508.4: a copy batch's tokens are seated at the destinations its
    /// request lists, in creation order.
    #[test]
    fn copy_batch_is_seated_at_each_requested_destination_in_order() {
        use crate::game::combat::{AttackTarget, CombatState};
        use crate::types::proposed_event::TokenAttackRequest;

        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 42);
        state.combat = Some(CombatState::default());
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
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
        let values =
            crate::game::printed_cards::intrinsic_copiable_values(&state.objects[&source_id]);
        let copy = CopyTokenSpec {
            values: Box::new(values),
            display: DisplayIdentity {
                display_source: DisplaySource::Token,
                ..Default::default()
            },
            extra_keywords: Vec::new(),
            additional_modifications: Vec::new(),
            tapped: true,
            enters_attacking: true,
            attack_request: TokenAttackRequest::Named {
                first: PlayerId(1),
                rest: vec![PlayerId(2)],
            },
            sacrifice_at: None,
            source_id,
            controller: PlayerId(0),
            donor: None,
        };
        let mut events = Vec::new();
        let status = apply_copy_token_after_replacement_with_created_ids(
            &mut state,
            None,
            None,
            PlayerId(0),
            copy,
            EtbTapState::Tapped,
            Vec::new(),
            2,
            Vec::new(),
            &mut events,
        );

        assert_eq!(
            status.created_ids.len(),
            2,
            "reach guard: both copies were created"
        );
        let seats: Vec<Option<AttackTarget>> = status
            .created_ids
            .iter()
            .map(|id| {
                state
                    .combat
                    .as_ref()
                    .unwrap()
                    .attackers
                    .iter()
                    .find(|a| a.object_id == *id)
                    .map(|a| a.attack_target)
            })
            .collect();
        assert_eq!(
            seats,
            vec![
                Some(AttackTarget::Player(PlayerId(1))),
                Some(AttackTarget::Player(PlayerId(2)))
            ]
        );
    }

    /// The attack target each created token holds in `state.combat`, in id order.
    fn seats_of_battlefield_tokens(
        state: &GameState,
    ) -> Vec<Option<crate::game::combat::AttackTarget>> {
        let mut token_ids: Vec<ObjectId> = state
            .objects
            .values()
            .filter(|obj| obj.is_token && obj.zone == Zone::Battlefield)
            .map(|obj| obj.id)
            .collect();
        token_ids.sort();
        token_ids
            .iter()
            .map(|id| {
                state
                    .combat
                    .as_ref()
                    .unwrap()
                    .attackers
                    .iter()
                    .find(|a| a.object_id == *id)
                    .map(|a| a.attack_target)
            })
            .collect()
    }

    /// A creature copy source for the attack-destination batch tests.
    fn add_copy_source(state: &mut GameState, name: &str) -> ObjectId {
        let source_id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
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
        source_id
    }

    fn named_attack_copy_spec(
        state: &GameState,
        source_id: ObjectId,
        additional_modifications: Vec<ContinuousModification>,
    ) -> CopyTokenSpec {
        use crate::types::proposed_event::TokenAttackRequest;

        CopyTokenSpec {
            values: Box::new(crate::game::printed_cards::intrinsic_copiable_values(
                &state.objects[&source_id],
            )),
            display: DisplayIdentity {
                display_source: DisplaySource::Token,
                ..Default::default()
            },
            extra_keywords: Vec::new(),
            additional_modifications,
            tapped: true,
            enters_attacking: true,
            attack_request: TokenAttackRequest::Named {
                first: PlayerId(1),
                rest: vec![PlayerId(2)],
            },
            sacrifice_at: None,
            source_id,
            controller: PlayerId(0),
            donor: None,
        }
    }

    /// CR 508.4: a non-liminal copy batch keeps its creation ordinal across a
    /// counter-replacement pause, so each token is still seated at the
    /// destination its own position in the request names.
    #[test]
    fn copy_batch_ordinal_survives_a_counter_pause() {
        use crate::game::combat::{AttackTarget, CombatState};

        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 42);
        state.combat = Some(CombatState::default());
        let replacement_source = create_object(
            &mut state,
            CardId(9),
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
        let source_id = add_copy_source(&mut state, "Grizzly Bears");
        let copy = named_attack_copy_spec(
            &state,
            source_id,
            vec![ContinuousModification::AddCounterOnEnter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                if_type: None,
            }],
        );

        let mut events = Vec::new();
        let status = apply_copy_token_after_replacement_with_created_ids(
            &mut state,
            None,
            None,
            PlayerId(0),
            copy,
            EtbTapState::Tapped,
            Vec::new(),
            3,
            Vec::new(),
            &mut events,
        );
        assert!(
            matches!(status.completion, CopyTokenApplyCompletion::Paused),
            "reach guard: the first token's counter placement paused the batch"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        for _ in 0..3 {
            if !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                break;
            }
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
        }

        assert_eq!(
            seats_of_battlefield_tokens(&state),
            vec![
                Some(AttackTarget::Player(PlayerId(1))),
                Some(AttackTarget::Player(PlayerId(2))),
                Some(AttackTarget::Player(PlayerId(1))),
            ]
        );
    }

    /// CR 508.4 + CR 614.1c: the same, when the pause is on the copied
    /// creature's own "enters with a counter" self-replacement.
    #[test]
    fn copy_batch_ordinal_survives_an_entry_counter_pause() {
        use crate::game::combat::{AttackTarget, CombatState};

        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 42);
        state.combat = Some(CombatState::default());
        let counter_choice = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Counter Choice".to_string(),
            Zone::Battlefield,
        );
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent);
            def.mode = crate::types::ability::ReplacementMode::Optional { decline: None };
            let obj = state.objects.get_mut(&counter_choice).unwrap();
            obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
            obj.replacement_definitions = vec![def].into();
        }
        let source_id = add_copy_source(&mut state, "Walking Ballista");
        {
            let enters_with_counter = ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::SelfRef,
                    },
                ));
            let source = state.objects.get_mut(&source_id).unwrap();
            source
                .replacement_definitions
                .push(enters_with_counter.clone());
            Arc::make_mut(&mut source.base_replacement_definitions).push(enters_with_counter);
        }
        let copy = named_attack_copy_spec(&state, source_id, Vec::new());
        assert!(
            !crate::game::printed_cards::self_etb_counter_replacements(
                &copy.values.replacement_definitions
            )
            .is_empty(),
            "reach guard: the copied values carry the entry counter"
        );

        let mut events = Vec::new();
        let status = apply_copy_token_after_replacement_with_created_ids(
            &mut state,
            None,
            None,
            PlayerId(0),
            copy,
            EtbTapState::Tapped,
            Vec::new(),
            3,
            Vec::new(),
            &mut events,
        );
        assert!(
            matches!(status.completion, CopyTokenApplyCompletion::Paused),
            "reach guard: the first token's entry counter paused the batch"
        );
        for _ in 0..3 {
            if !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                break;
            }
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
        }

        let seats = seats_of_battlefield_tokens(&state);
        assert_eq!(
            seats,
            vec![
                Some(AttackTarget::Player(PlayerId(1))),
                Some(AttackTarget::Player(PlayerId(2))),
                Some(AttackTarget::Player(PlayerId(1))),
            ]
        );
    }

    /// CR 508.4 + CR 614.12: a liminal copy batch whose entry pauses on its
    /// enter-as-a-copy target's own entry counters keeps its creation ordinal
    /// for the tokens parked behind the pause.
    #[test]
    fn copy_batch_ordinal_survives_a_copy_target_entry_counter_pause() {
        use crate::game::combat::{AttackTarget, CombatState};

        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 42);
        state.combat = Some(CombatState::default());
        let counter_choice = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Counter Choice".to_string(),
            Zone::Battlefield,
        );
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent);
            def.mode = crate::types::ability::ReplacementMode::Optional { decline: None };
            let obj = state.objects.get_mut(&counter_choice).unwrap();
            obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
            obj.replacement_definitions = vec![def].into();
        }
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cursed Mirror".to_string(),
            Zone::Battlefield,
        );
        {
            let mirror = state.objects.get_mut(&source_id).unwrap();
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
        // CR 614.1c: the creature the token becomes enters with a counter.
        let target_id = add_copy_source(&mut state, "Walking Ballista");
        {
            let enters_with_counter = ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::SelfRef,
                    },
                ));
            let target = state.objects.get_mut(&target_id).unwrap();
            target
                .replacement_definitions
                .push(enters_with_counter.clone());
            Arc::make_mut(&mut target.base_replacement_definitions).push(enters_with_counter);
        }
        let copy = named_attack_copy_spec(&state, source_id, Vec::new());
        let mut denied_state = state.clone();
        let denied_copy = copy.clone();

        let mut events = Vec::new();
        let status = apply_copy_token_after_replacement_with_created_ids(
            &mut state,
            None,
            None,
            PlayerId(0),
            copy,
            EtbTapState::Tapped,
            Vec::new(),
            2,
            Vec::new(),
            &mut events,
        );
        assert!(
            matches!(status.completion, CopyTokenApplyCompletion::Paused),
            "reach guard: the first token's entry paused the batch"
        );
        let mut counter_pauses = 0;
        for _ in 0..12 {
            match state.waiting_for.clone() {
                WaitingFor::ReplacementChoice { .. } => {
                    apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                        .unwrap();
                }
                WaitingFor::CopyTargetChoice { .. } => {
                    apply_as_current(
                        &mut state,
                        GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(target_id)),
                        },
                    )
                    .unwrap();
                    if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
                        counter_pauses += 1;
                    }
                }
                _ => break,
            }
        }
        assert!(
            counter_pauses > 0,
            "reach guard: a copy target's entry counters paused the batch"
        );

        assert_eq!(
            seats_of_battlefield_tokens(&state),
            vec![
                Some(AttackTarget::Player(PlayerId(1))),
                Some(AttackTarget::Player(PlayerId(2))),
            ]
        );

        // CR 614.6: the first selected entry is prevented through the public
        // response. Its denied attempt must leave P1 for the remaining birth.
        let mut denied_events = Vec::new();
        let status = apply_copy_token_after_replacement_with_created_ids(
            &mut denied_state,
            None,
            None,
            PlayerId(0),
            denied_copy,
            EtbTapState::Tapped,
            Vec::new(),
            2,
            Vec::new(),
            &mut denied_events,
        );
        assert!(matches!(
            status.completion,
            CopyTokenApplyCompletion::Paused
        ));
        assert!(matches!(
            denied_state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
        denied_events.extend(
            apply_as_current(
                &mut denied_state,
                GameAction::ChooseReplacement { index: 0 },
            )
            .unwrap()
            .events,
        );
        let WaitingFor::CopyTargetChoice {
            source_id: denied_entrant,
            player: PlayerId(0),
            ref valid_targets,
            ..
        } = denied_state.waiting_for
        else {
            panic!("first copy entry must reach its public target choice");
        };
        assert!(valid_targets.contains(&target_id));
        let prevention_source = ObjectId(denied_state.next_object_id);
        install_test_replacement(
            &mut denied_state,
            "Ordinal entry prevention",
            ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent)
                .mode(ReplacementMode::Optional { decline: None }),
        );
        denied_events.extend(
            apply_as_current(
                &mut denied_state,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target_id)),
                },
            )
            .unwrap()
            .events,
        );
        assert!(matches!(
            denied_state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
        let pending = denied_state.pending_replacement.as_ref().unwrap();
        assert!(pending.is_optional);
        assert_eq!(pending.candidates.len(), 1);
        assert_eq!(pending.candidates[0].source, prevention_source);
        let opportunity = Default::default();
        let definition = crate::game::replacement::replacement_definition_for_key(
            &denied_state,
            pending.candidate_keys[0],
            &opportunity,
        )
        .unwrap();
        assert_eq!(definition.event, ReplacementEvent::ChangeZone);
        assert_eq!(
            definition.quantity_modification,
            Some(QuantityModification::Prevent)
        );
        let entry = &denied_state.liminal_entries[&denied_entrant];
        assert_eq!(entry.installed_copy_effects.len(), 1);
        let pin = ObjectIncarnationRef::from_object(entry.object.projected());
        assert!(denied_state
            .transient_continuous_effects
            .iter()
            .any(
                |effect| effect.id == entry.installed_copy_effects[0].continuous_effect_id
                    && effect.affected_recipient == Some(pin)
            ));
        denied_events.extend(
            apply_as_current(
                &mut denied_state,
                GameAction::ChooseReplacement { index: 0 },
            )
            .unwrap()
            .events,
        );
        assert!(!denied_state.objects.contains_key(&denied_entrant));
        for _ in 0..20 {
            let action = match &denied_state.waiting_for {
                WaitingFor::ReplacementChoice {
                    player: PlayerId(0),
                    ..
                } => {
                    let pending = denied_state.pending_replacement.as_ref().unwrap();
                    GameAction::ChooseReplacement {
                        index: usize::from(
                            pending.is_optional
                                && pending.candidates[0].source == prevention_source,
                        ),
                    }
                }
                WaitingFor::CopyTargetChoice {
                    player: PlayerId(0),
                    valid_targets,
                    ..
                } => {
                    assert!(valid_targets.contains(&target_id));
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target_id)),
                    }
                }
                WaitingFor::Priority { .. } if denied_state.resolution_stack.is_empty() => break,
                other => panic!("remaining ordinal entry: {other:?}"),
            };
            denied_events.extend(apply_as_current(&mut denied_state, action).unwrap().events);
        }
        assert_eq!(
            seats_of_battlefield_tokens(&denied_state),
            vec![Some(AttackTarget::Player(PlayerId(1)))]
        );
        assert_eq!(
            denied_events
                .iter()
                .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                .count(),
            1
        );
        assert!(denied_events.iter().all(|event| !matches!(event,
            GameEvent::TokenCreated { object_id, .. } | GameEvent::ZoneChanged { object_id, .. }
                if *object_id == denied_entrant)));
        assert!(denied_state.liminal_entries.is_empty());
        assert!(denied_state.resolution_stack.is_empty());
        assert!(denied_state.pending_replacement.is_none());
        assert!(denied_state.pending_liminal_entry_resume.is_none());
        assert!(matches!(
            denied_state.waiting_for,
            WaitingFor::Priority { .. }
        ));
        for completed in [&state, &denied_state] {
            assert!(completed.stack.is_empty());
            assert!(completed.resolution_stack.is_empty());
            assert!(completed.return_result_frames.is_empty());
            assert!(completed.active_return_result_occurrence.is_none());
            assert!(completed.resolving_stack_entry.is_none());
            assert!(completed.pending_resolution_completion.is_none());
            assert!(completed.pending_liminal_entry_resume.is_none());
            assert!(completed.liminal_entries.is_empty());
            assert!(completed.pending_replacement.is_none());
            assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
        }
    }

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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                        attack_destination:
                            crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        eprintln!(
            "name-only source before resolution: identity={:?}; source={:?}",
            state.objects[&source_id].display_identity(),
            state.objects[&source_id]
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token_id = ObjectId(state.next_object_id - 1);
        let token = state.objects.get(&token_id).unwrap();
        eprintln!("name-only copy after resolution: identity={:?}; printed={:?}; art={:?}; body={token:?}; events={events:?}",
            token.display_identity(), token.base_printed_ref, token.token_image_ref);
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

    #[test]
    fn twinflame_public_name_only_copy_preserves_card_baseline_through_flush_and_cleanup() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;
        const TWINFLAME: &str = "Strive — This spell costs {2}{R} more to cast for each target beyond the first.\nChoose any number of target creatures you control. For each of them, create a token that's a copy of that creature, except it has haste. Exile those tokens at the beginning of the next end step.";
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let donor = scenario.add_creature(P0, "Fanatic of Rhonas", 4, 4).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Twinflame", false, TWINFLAME)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, false, false, vec![]); 2],
        );
        let mut runner = scenario.build();
        {
            // Deliberately synthetic, ref-less 4/4 body; the printed Fanatic's
            // name does not supply catalog provenance or change this body.
            let object = runner.state_mut().objects.get_mut(&donor).unwrap();
            object.color = vec![ManaColor::Black];
            object.base_color = object.color.clone();
            object.card_types.subtypes = vec!["Zombie".into(), "Snake".into(), "Druid".into()];
            object.base_card_types = object.card_types.clone();
        }
        let original = runner.state().objects[&donor].display_identity();
        let outcome = runner.cast(spell).target_object(donor).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        let births: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(births.len(), 1);
        let token = births[0];
        let pin = ObjectIncarnationRef::from_object(&runner.state().objects[&token]);
        for _ in 0..3 {
            crate::game::layers::evaluate_layers(runner.state_mut());
            crate::game::layers::mark_layers_entered(runner.state_mut(), token);
            crate::game::layers::flush_layers(runner.state_mut());
            let object = &runner.state().objects[&token];
            // CR 111.1 + CR 707.2 + CR 707.9a: a created token copies the
            // synthetic body and gains haste, independently of display routing.
            assert!(object.is_token);
            assert_eq!(object.name, "Fanatic of Rhonas");
            assert_eq!((object.power, object.toughness), (Some(4), Some(4)));
            assert_eq!(object.color, vec![ManaColor::Black]);
            assert_eq!(object.card_types.subtypes, vec!["Zombie", "Snake", "Druid"]);
            assert!(object.has_keyword(&Keyword::Haste));
            assert_eq!(object.display_source, DisplaySource::Card);
            assert_eq!(object.base_display_source, Some(DisplaySource::Card));
            assert!(object.printed_ref.is_none());
            assert!(object.base_printed_ref.is_none());
            assert!(object.token_image_ref.is_none());
            assert!(object.base_token_image_ref.is_none());
        }
        assert_eq!(runner.state().objects[&donor].display_identity(), original);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
        assert!(runner.state().liminal_entries.is_empty());
        assert!(runner
            .state()
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.ability.target_incarnations.contains(&pin)));
        let mut departures = Vec::new();
        for _ in 0..160 {
            if !runner.state().battlefield.contains(&token)
                && runner.state().stack.is_empty()
                && runner.state().resolution_stack.is_empty()
            {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                _ => GameAction::PassPriority,
            };
            departures.extend(runner.act(action).unwrap().events);
        }
        // CR 603.7c: observe the real delayed exile before token cessation.
        assert_eq!(departures.iter().filter(|event| matches!(event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Exile, record }
            if *object_id == token && record.trigger_source_context.as_ref().is_some_and(|context| context.identity.reference == pin)
        )).count(), 1);
        assert!(!runner.state().battlefield.contains(&token));
        assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }

    #[test]
    fn printed_mirror_public_token_display_accept_decline_no_source_and_expiry() {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::types::card::{PrintedCardRef, TokenImageRef};
        use crate::types::phase::Phase;
        for (has_donor, accept) in [(true, true), (true, false), (false, true)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let mirror = scenario
                .add_artifact_to_hand_from_oracle(P0, "Cursed Mirror", PRINTED_CURSED_MIRROR)
                .id();
            let donor = has_donor.then(|| {
                scenario
                    .add_creature(P1, "Synthetic native Rabbit", 2, 3)
                    .id()
            });
            let own_ref = PrintedCardRef {
                oracle_id: "mirror-original-face".into(),
                face_name: "Cursed Mirror".into(),
            };
            let image = TokenImageRef {
                scryfall_id: "native-rabbit-image".into(),
                scryfall_oracle_id: None,
                face_name: None,
                preset_id: "native-rabbit".into(),
            };
            let mut runner = scenario.build();
            {
                let object = runner.state_mut().objects.get_mut(&mirror).unwrap();
                object.printed_ref = Some(own_ref.clone());
                object.base_printed_ref = Some(own_ref.clone());
            }
            if let Some(donor) = donor {
                let object = runner.state_mut().objects.get_mut(&donor).unwrap();
                object.is_token = true;
                object.install_display_identity_as_base(&DisplayIdentity {
                    display_source: DisplaySource::Token,
                    printed_ref: None,
                    token_image_ref: Some(image.clone()),
                    token_art: None,
                });
                object.restore_token_art_baseline();
            }
            runner.cast(mirror).commit();
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            if let Some(donor) = donor {
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P0, .. }
                ));
                assert!(
                    runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .unwrap()
                        .is_optional
                );
                runner
                    .act(GameAction::ChooseReplacement {
                        index: if accept { 0 } else { 1 },
                    })
                    .unwrap();
                if accept {
                    assert!(matches!(&runner.state().waiting_for,
                        WaitingFor::CopyTargetChoice { player: P0, valid_targets, .. }
                        if valid_targets.contains(&donor)));
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(donor)),
                        })
                        .unwrap();
                }
            }
            crate::game::layers::flush_layers(runner.state_mut());
            let object = &runner.state().objects[&mirror];
            assert!(!object.is_token);
            assert_eq!(object.zone, Zone::Battlefield);
            assert_eq!(object.base_display_source, Some(DisplaySource::Card));
            assert_eq!(object.base_printed_ref, Some(own_ref.clone()));
            assert_eq!(object.base_token_image_ref, None);
            if has_donor && accept {
                assert_eq!(object.display_source, DisplaySource::Token);
                assert_eq!(object.token_image_ref, Some(image.clone()));
                assert_eq!((object.power, object.toughness), (Some(2), Some(3)));
                assert!(object.has_keyword(&Keyword::Haste));
            } else {
                assert_eq!(object.name, "Cursed Mirror");
                assert_eq!(object.display_source, DisplaySource::Card);
                assert_eq!(object.token_image_ref, None);
                assert_eq!(object.card_types.core_types, vec![CoreType::Artifact]);
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            for state in liminal_copy_reader_forms(runner.state()) {
                let mut restored = GameRunner::from_state(state);
                for _ in 0..160 {
                    if restored.state().turn_number > runner.state().turn_number {
                        break;
                    }
                    let action = match restored.state().waiting_for {
                        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        },
                        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                        _ => GameAction::PassPriority,
                    };
                    restored.act(action).unwrap();
                }
                assert!(restored.state().turn_number > runner.state().turn_number);
                let object = &restored.state().objects[&mirror];
                assert_eq!(object.name, "Cursed Mirror");
                assert_eq!(object.card_types.core_types, vec![CoreType::Artifact]);
                assert_eq!(object.display_source, DisplaySource::Card);
                assert_eq!(object.printed_ref, Some(own_ref.clone()));
                assert_eq!(object.token_image_ref, None);
                assert!(restored.state().stack.is_empty());
                assert!(restored.state().resolution_stack.is_empty());
                assert!(matches!(
                    restored.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
            }
        }
    }

    #[test]
    fn public_copy_preset_finalization_installs_exact_baseline_without_legacy_guessing() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::phase::Phase;
        const PRESET: &str = "001dd45c-851b-5eb3-9a53-fc9fb2c0e322";
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Fanatic of Rhonas", 4, 4)
            .with_ability_definition(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::CopyTokenOf {
                    target: TargetFilter::SelfRef,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    extra_keywords: vec![],
                    additional_modifications: vec![],
                },
            ))
            .id();
        let mut runner = scenario.build();
        {
            // Synthetic activation; independently supplied printed source and
            // exact related-token link exercise the existing preset authority.
            let object = runner.state_mut().objects.get_mut(&source).unwrap();
            object.printed_ref = Some(PrintedCardRef {
                oracle_id: "7973820b-fdaf-46ec-9e3e-d4c0e77b5067".into(),
                face_name: "Fanatic of Rhonas".into(),
            });
            object.base_printed_ref = object.printed_ref.clone();
            object.source_related_token_ids = vec![PRESET.into()];
            object.base_color = vec![ManaColor::Black];
            object.color = object.base_color.clone();
            object.base_card_types.subtypes = vec!["Zombie".into(), "Snake".into(), "Druid".into()];
            object.card_types = object.base_card_types.clone();
        }
        let result = runner.activate(source, 0).resolve();
        assert!(matches!(
            result.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        let births: Vec<_> = result
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(births.len(), 1);
        let id = births[0];
        for _ in 0..3 {
            crate::game::layers::evaluate_layers(runner.state_mut());
            let object = &runner.state().objects[&id];
            assert!(object.is_token);
            assert_eq!((object.power, object.toughness), (Some(4), Some(4)));
            assert_eq!(object.display_source, DisplaySource::Token);
            assert_eq!(object.base_display_source, Some(DisplaySource::Token));
            assert_eq!(
                object
                    .token_image_ref
                    .as_ref()
                    .map(|image| image.preset_id.as_str()),
                Some(PRESET)
            );
            assert_eq!(
                object
                    .base_token_image_ref
                    .as_ref()
                    .map(|image| image.scryfall_id.as_str()),
                Some("6ef58164-4155-4e5b-8c16-f16f2ab65baa")
            );
        }
        let mut legacy = serde_json::to_value(runner.state()).unwrap();
        let object = legacy["objects"][id.0.to_string()].as_object_mut().unwrap();
        object.remove("base_display_source");
        object.remove("base_token_image_ref");
        let legacy: GameState = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            legacy.objects[&id].base_display_source, None,
            "the initial Card birth cannot prove this later Token preset baseline"
        );
        assert!(legacy.stack.is_empty());
        assert!(legacy.resolution_stack.is_empty());
    }

    #[test]
    fn native_token_finite_public_copy_restores_own_exact_image_after_all_readers() {
        use crate::game::scenario::{GameRunner, GameScenario, P0};
        use crate::types::card::TokenImageRef;
        use crate::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // Explicit synthetic activation exercises the ordinary public pipeline;
        // this fixture makes no printed-card support claim.
        let first = scenario
            .add_creature(P0, "Native T1", 2, 3)
            .with_ability_definition(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::BecomeCopy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    recipient: crate::types::ability::CopyRecipient::Source,
                    duration: Some(Duration::UntilEndOfTurn),
                    mana_value_limit: None,
                    additional_modifications: vec![],
                },
            ))
            .id();
        let second = scenario.add_creature(P0, "Native T2", 5, 6).id();
        let image1 = TokenImageRef {
            scryfall_id: "T1-exact-image".into(),
            scryfall_oracle_id: None,
            face_name: None,
            preset_id: "T1-preset".into(),
        };
        let image2 = TokenImageRef {
            scryfall_id: "T2-exact-image".into(),
            scryfall_oracle_id: None,
            face_name: None,
            preset_id: "T2-preset".into(),
        };
        let mut runner = scenario.build();
        for (id, image) in [(first, &image1), (second, &image2)] {
            let object = runner.state_mut().objects.get_mut(&id).unwrap();
            object.is_token = true;
            object.install_display_identity_as_base(&DisplayIdentity {
                display_source: DisplaySource::Token,
                printed_ref: None,
                token_image_ref: Some(image.clone()),
                token_art: None,
            });
            object.restore_token_art_baseline();
        }
        let outcome = runner.activate(first, 0).target_object(second).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert_eq!(outcome.state().objects[&first].name, "Native T2");
        assert_eq!(
            outcome.state().objects[&first].token_image_ref,
            Some(image2.clone())
        );
        assert_eq!(
            outcome.state().objects[&first].base_token_image_ref,
            Some(image1.clone())
        );
        assert!(outcome.state().objects[&first].layer1_copy_effect.is_some());
        assert!(outcome.state().objects[&first].is_token);
        let mut forms: Vec<_> = liminal_copy_reader_forms(runner.state())
            .into_iter()
            .map(|state| (state, true))
            .collect();
        let mut old_active = serde_json::to_value(runner.state()).unwrap();
        let old_object = old_active["objects"][first.0.to_string()]
            .as_object_mut()
            .unwrap();
        old_object.remove("base_display_source");
        old_object.remove("base_token_image_ref");
        forms.push((serde_json::from_value(old_active).unwrap(), false));
        for (mut state, known) in forms {
            for _ in 0..3 {
                crate::game::layers::evaluate_layers(&mut state);
                crate::game::layers::mark_layers_entered(&mut state, first);
                crate::game::layers::flush_layers(&mut state);
                assert_eq!(state.objects[&first].token_image_ref, Some(image2.clone()));
                assert_eq!(
                    state.objects[&first].base_token_image_ref,
                    known.then(|| image1.clone())
                );
                assert_eq!(
                    state.objects[&first].base_display_source,
                    known.then_some(DisplaySource::Token)
                );
            }
            let old_turn = state.turn_number;
            let mut restored = GameRunner::from_state(state);
            for _ in 0..160 {
                if restored.state().turn_number > old_turn {
                    break;
                }
                let action = match restored.state().waiting_for {
                    WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    },
                    WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                        assignments: vec![],
                    },
                    _ => GameAction::PassPriority,
                };
                restored.act(action).unwrap();
            }
            assert!(restored.state().turn_number > old_turn);
            let object = &restored.state().objects[&first];
            assert_eq!(object.name, "Native T1");
            assert_eq!((object.power, object.toughness), (Some(2), Some(3)));
            assert_eq!(object.display_source, DisplaySource::Token);
            assert_eq!(
                object.token_image_ref,
                Some(if known {
                    image1.clone()
                } else {
                    image2.clone()
                })
            );
            assert_eq!(object.base_token_image_ref, known.then(|| image1.clone()));
            assert_eq!(
                object.base_display_source,
                known.then_some(DisplaySource::Token)
            );
            let again: GameState =
                serde_json::from_value(serde_json::to_value(restored.state()).unwrap()).unwrap();
            assert_eq!(
                again.objects[&first].base_display_source,
                known.then_some(DisplaySource::Token)
            );
            assert!(restored.state().stack.is_empty());
            assert!(restored.state().resolution_stack.is_empty());
            assert!(matches!(
                restored.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }

    #[test]
    fn legacy_display_birth_evidence_and_actual_turn_truncation_remain_honest() {
        use crate::game::scenario::{GameRunner, GameScenario, P0};
        use crate::types::phase::Phase;
        use crate::types::resolved_commands::ResolvedRulesCommand;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let donor = scenario.add_creature(P0, "Fanatic of Rhonas", 4, 4).id();
        let factory = scenario
            .add_artifact_from_oracle(P0, "Synthetic birth factory", "")
            .with_ability_definition(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Token {
                    name: "Fanatic of Rhonas".into(),
                    power: PtValue::Fixed(4),
                    toughness: PtValue::Fixed(4),
                    types: vec![
                        "Creature".into(),
                        "Zombie".into(),
                        "Snake".into(),
                        "Druid".into(),
                    ],
                    colors: vec![ManaColor::Black],
                    keywords: vec![Keyword::Haste],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
            ))
            .with_ability_definition(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::CopyTokenOf {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    extra_keywords: vec![],
                    additional_modifications: vec![],
                },
            ))
            .id();
        let mut runner = scenario.build();
        {
            let object = runner.state_mut().objects.get_mut(&donor).unwrap();
            object.base_color = vec![ManaColor::Black];
            object.color = object.base_color.clone();
            object.base_card_types.subtypes = vec!["Zombie".into(), "Snake".into(), "Druid".into()];
            object.card_types = object.base_card_types.clone();
            object.base_keywords = vec![Keyword::Haste];
            object.keywords = object.base_keywords.clone();
        }
        let native_result = runner.activate(factory, 0).resolve();
        assert!(matches!(
            native_result.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        let native: Vec<_> = native_result
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(native.len(), 1);
        let native = native[0];
        let copy_result = runner.activate(factory, 1).target_object(donor).resolve();
        assert!(matches!(
            copy_result.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        let copied: Vec<_> = copy_result
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(copied.len(), 1);
        let copied = copied[0];
        assert_eq!(
            crate::game::printed_cards::intrinsic_copiable_values(&runner.state().objects[&native]),
            crate::game::printed_cards::intrinsic_copiable_values(&runner.state().objects[&copied])
        );
        assert_eq!(
            runner.state().objects[&native].base_display_source,
            Some(DisplaySource::Token)
        );
        assert_eq!(
            runner.state().objects[&copied].base_display_source,
            Some(DisplaySource::Card)
        );
        assert!(runner.state().resolved_rules_journal.entries().iter().any(|entry|
            matches!(&entry.command, Some(ResolvedRulesCommand::TokenCreation(command)) if command.object.object_id == native)));
        let erase = |state: &GameState| {
            let mut value = serde_json::to_value(state).unwrap();
            for id in [native, copied] {
                let object = value["objects"][id.0.to_string()].as_object_mut().unwrap();
                object.remove("base_display_source");
                object.remove("base_token_image_ref");
            }
            value
        };
        let legacy: GameState = serde_json::from_value(erase(runner.state())).unwrap();
        assert_eq!(
            legacy.objects[&native].base_display_source,
            Some(DisplaySource::Token)
        );
        // Initial Card copy display alone does not prove post-birth preset finalization.
        assert_eq!(legacy.objects[&copied].base_display_source, None);
        let mut wrong = erase(runner.state());
        let mut changed = 0;
        for entry in wrong["resolved_rules_journal"]["entries"]
            .as_array_mut()
            .unwrap()
        {
            if let Some(command) = entry.pointer_mut("/command/TokenCreation") {
                if command["object"]["object_id"] == native.0 {
                    command["object"]["incarnation"] =
                        serde_json::json!(runner.state().objects[&native].incarnation + 1);
                    changed += 1;
                }
            }
        }
        assert_eq!(changed, 1, "the wrong-incarnation witness really changed");
        let wrong: GameState = serde_json::from_value(wrong).unwrap();
        assert_eq!(wrong.objects[&native].base_display_source, None);
        assert!(wrong.objects[&native].is_token);
        assert_eq!(
            (
                wrong.objects[&native].power,
                wrong.objects[&native].toughness
            ),
            (Some(4), Some(4))
        );
        let old_turn = runner.state().turn_number;
        for _ in 0..160 {
            if runner.state().turn_number > old_turn {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                _ => GameAction::PassPriority,
            };
            runner.act(action).unwrap();
        }
        assert!(runner.state().turn_number > old_turn);
        assert!(runner
            .state()
            .resolved_rules_journal
            .entries()
            .iter()
            .all(|entry| !matches!(&entry.command, Some(ResolvedRulesCommand::TokenCreation(_)))));
        for state in liminal_copy_reader_forms(runner.state()) {
            assert_eq!(
                state.objects[&native].base_display_source,
                Some(DisplaySource::Token)
            );
            assert_eq!(
                state.objects[&copied].base_display_source,
                Some(DisplaySource::Card)
            );
        }
        let mut legacy: GameState = serde_json::from_value(erase(runner.state())).unwrap();
        for _ in 0..3 {
            crate::game::layers::evaluate_layers(&mut legacy);
            for id in [native, copied] {
                assert_eq!(legacy.objects[&id].base_display_source, None);
                assert_eq!(legacy.objects[&id].base_token_image_ref, None);
                assert_eq!(legacy.objects[&id].display_source, DisplaySource::Token);
                assert!(legacy.objects[&id].is_token);
            }
            legacy = serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
        }
        for restored in liminal_copy_reader_forms(&legacy) {
            for id in [native, copied] {
                assert_eq!(restored.objects[&id].base_display_source, None);
                assert_eq!(restored.objects[&id].base_token_image_ref, None);
            }
        }
        // A genuine new copy may bind the presented fallback for its own birth;
        // it must not write proof back into the old donor's unknown baseline.
        let mut legacy = GameRunner::from_state(legacy);
        if matches!(legacy.state().waiting_for, WaitingFor::Priority { player } if player != P0) {
            legacy.act(GameAction::PassPriority).unwrap();
        }
        let result = legacy.activate(factory, 1).target_object(copied).resolve();
        assert!(matches!(
            result.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        let born: Vec<_> = result
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();
        assert_eq!(born.len(), 1);
        assert_eq!(
            legacy.state().objects[&born[0]].base_display_source,
            Some(DisplaySource::Token)
        );
        assert_eq!(legacy.state().objects[&copied].base_display_source, None);
        let mut saved = serde_json::to_value(legacy.state()).unwrap();
        let object = saved["objects"][born[0].0.to_string()]
            .as_object_mut()
            .unwrap();
        object.remove("base_display_source");
        object.remove("base_token_image_ref");
        let recovered: GameState = serde_json::from_value(saved).unwrap();
        assert_eq!(
            recovered.objects[&born[0]].base_display_source,
            Some(DisplaySource::Token)
        );
        assert_eq!(recovered.objects[&copied].base_display_source, None);
        assert!(legacy.state().stack.is_empty());
        assert!(legacy.state().resolution_stack.is_empty());
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                binding: None,
                object_id: artifact_id,
                lki: artifact.snapshot_for_mana_spent(),
                incarnation: 0,
            }
        };
        let mut ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::CostPaidObject {
                    reference: crate::types::ability::CostPaidObjectRef::Selected,
                },
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
            source.base_card_types.core_types = vec![CoreType::Creature];
            source.card_types = source.base_card_types.clone();
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        assert_eq!(token.name, "Silver Myr");
        assert_eq!(token.base_name, "Mishra's Warform");
        assert_eq!(token.power, Some(1));
        assert_eq!(token.base_power, Some(4));
        assert_eq!(token.toughness, Some(1));
        assert_eq!(token.base_toughness, Some(4));
        assert!(token.card_types.core_types.contains(&CoreType::Artifact));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.contains(&"Myr".to_string()));
        assert!(token
            .base_card_types
            .subtypes
            .contains(&"Construct".to_string()));
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
            let attacker = state.objects.get_mut(&source).unwrap();
            attacker.controller = PlayerId(1);
            attacker.base_controller = Some(PlayerId(1));
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
            // allow-raw-zone: test setup places the Encore source in exile before resolution; no gameplay move is simulated.
            state.battlefield.retain(|id| *id != source);
            // allow-raw-zone: test setup assigns the Encore source's initial exile zone before resolution.
            state.objects.get_mut(&source).unwrap().zone = Zone::Exile;
            // allow-raw-zone: test setup registers the initial Encore source in the exile container.
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
                player: TargetFilter::Controller,
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        assert!(!copy_token_uses_liminal_entry(copy));
        let mut immediate = copy.clone();
        immediate.additional_modifications.clear();
        assert!(copy_token_uses_liminal_entry(&immediate));
        assert_eq!(
            copy_token_entry_counters(&immediate, &[(CounterType::Plus1Plus1, 1)]),
            vec![(CounterType::Plus1Plus1, 1)]
        );
        immediate.values.loyalty = Some(4);
        immediate
            .additional_modifications
            .push(ContinuousModification::SetStartingLoyalty { value: 2 });
        assert!(copy_token_uses_liminal_entry(&immediate));
        assert_eq!(
            copy_token_entry_counters(&immediate, &[]),
            vec![(CounterType::Loyalty, 2)]
        );
        immediate.values.loyalty = None;
        immediate.additional_modifications = vec![ContinuousModification::RemoveManaCost];
        assert!(copy_token_uses_liminal_entry(&immediate));
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
        immediate.values.replacement_definitions = parsed_counter.replacements.into();
        assert!(copy_token_uses_liminal_entry(&immediate));
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
        )
        .expect("token creation fixture admits its complete program");
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
                                attack_destination:
                                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                                attack_destination:
                                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                    assert!(delayed.one_shot);
                    assert_eq!(state.delayed_triggers.len(), 1);
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
                    let unchanged_source = serde_json::to_value(&state.objects[&source]).unwrap();
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
                        // CR 508.1 + CR 509.1: combat declarations are turn-based
                        // actions, so passing priority cannot answer them.
                        let action = match runner.state().waiting_for {
                            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                                attacks: vec![],
                                bands: vec![],
                            },
                            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                                assignments: vec![],
                            },
                            _ => GameAction::PassPriority,
                        };
                        events.extend(runner.act(action).unwrap().events);
                    }
                    assert!(!runner.state().stack.is_empty());
                    let delayed_entry = runner
                        .state()
                        .stack
                        .iter()
                        .find(|entry| entry.source_id == source)
                        .unwrap();
                    assert_eq!(delayed_entry.ability().unwrap().target_incarnations, births);
                    for _ in 0..40 {
                        if runner.state().stack.is_empty()
                            && runner.state().resolution_stack.is_empty()
                        {
                            break;
                        }
                        assert!(matches!(
                            runner.state().waiting_for,
                            WaitingFor::Priority { .. }
                        ));
                        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                    }
                    for member in &births {
                        // CR 701.21a: the public sacrifice moves this exact birth
                        // from the battlefield to its owner's graveyard once.
                        assert_eq!(events.iter().filter(|event| matches!(event,
                            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, record }
                            if *object_id == member.object_id && record.trigger_source_context.as_ref().is_some_and(|context| context.identity.reference == *member)
                        )).count(), 1);
                        assert_eq!(events.iter().filter(|event| matches!(event,
                            GameEvent::PermanentSacrificed { object_id, player_id: PlayerId(0) }
                            if *object_id == member.object_id
                        )).count(), 1);
                    }
                    for member in &births {
                        // CR 111.7 + CR 704.5d: off-battlefield tokens cease to exist.
                        assert!(!member.is_current(runner.state()));
                        assert!(!runner.state().objects.contains_key(&member.object_id));
                        assert!(!runner.state().battlefield.contains(&member.object_id));
                    }
                    assert!(runner.state().stack.is_empty());
                    assert!(runner.state().resolution_stack.is_empty());
                    assert!(runner.state().return_result_frames.is_empty());
                    assert!(runner.state().active_return_result_occurrence.is_none());
                    assert!(runner.state().resolving_stack_entry.is_none());
                    assert!(runner.state().pending_resolution_completion.is_none());
                    assert!(runner.state().pending_liminal_entry_resume.is_none());
                    assert!(runner.state().liminal_entries.is_empty());
                    assert!(runner.state().pending_replacement.is_none());
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    // CR 603.7b: the registered one-shot trigger was consumed.
                    assert!(runner.state().delayed_triggers.is_empty());
                    assert_eq!(
                        serde_json::to_value(&runner.state().objects[&source]).unwrap(),
                        unchanged_source
                    );
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        )
        .expect("token creation fixture admits its complete program");
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
        assert!(serde_json::from_value::<ResolutionStateWire>(good.clone()).is_ok());
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
            let mut bad = good.clone();
            let owner = bad["resolution_frames"]["frames"][0]["data"]
                .as_object_mut()
                .expect("actual serialized creation frame");
            match fault {
                0 => {
                    owner.insert("creation_id".into(), serde_json::json!(0));
                }
                1 => {
                    owner.insert("result_parent".into(), serde_json::json!(id.0 + 99));
                }
                2 => {
                    owner.insert("creator".into(), serde_json::Value::Null);
                }
                3 => {
                    owner.insert("segment_start".into(), serde_json::json!(2));
                }
                4 => {
                    owner.insert("replacement_queue_tail".into(), serde_json::json!(0));
                }
                5 => {
                    owner
                        .entry("remaining")
                        .or_insert_with(|| serde_json::json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::to_value(&legacy).unwrap());
                }
                _ => {
                    owner
                        .get_mut("created_incarnations")
                        .unwrap()
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::to_value(member).unwrap());
                }
            };
            assert!(
                serde_json::from_value::<ResolutionStateWire>(bad).is_err(),
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
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                    player: TargetFilter::Controller,
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
                    attack_destination:
                        crate::types::ability::TokenAttackDestination::ControllerChooses,
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
                // allow-raw-zone: hostile reload fixture removes the paused entrant to exercise missing-object continuation handling.
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        )
        .expect("token creation fixture admits its complete program");
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
    pub(crate) fn liminal_copy_reader_forms(state: &GameState) -> Vec<GameState> {
        use crate::types::game_state::{PersistedGameState, TrustedGameStateEnvelope};
        use crate::types::resolution::ResolutionFrame;
        let mut forms = vec![
            state.clone(),
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap(),
            serde_json::from_value::<TrustedGameStateEnvelope>(
                serde_json::to_value(TrustedGameStateEnvelope::capture(state.clone())).unwrap(),
            )
            .unwrap()
            .into_game_state(),
            PersistedGameState::capture(state.clone())
                .into_game_state()
                .unwrap(),
            serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap(),
            )
            .unwrap()
            .into_game_state(),
        ];
        let nested = state.resolution_stack.iter().any(|frame| {
            matches!(
                frame,
                ResolutionFrame::MultiDraw(_) | ResolutionFrame::DebugCardEntries(_)
            )
        }) || state
            .resolution_stack
            .iter()
            .filter(|frame| matches!(frame, ResolutionFrame::CopyToken(_)))
            .count()
            > 1
            || state.resolution_stack.ability_continuations().count() > 1
            || state.objects.values().any(|object| {
                object
                    .ordinary_entry_counter_work
                    .as_ref()
                    .is_some_and(|work| work.dispatch_frame.is_none())
            });
        if nested {
            let mut retained = serde_json::to_value(state).unwrap();
            retained["resolution_state_version"] = serde_json::json!(1);
            forms.push(
                serde_json::from_value::<ResolutionStateWire>(retained)
                    .unwrap()
                    .into_game_state(),
            );
        } else {
            let mut legacy = serde_json::to_value(state).unwrap();
            legacy.as_object_mut().unwrap().remove("resolution_stack");
            legacy["resolution_state_version"] = serde_json::json!(1);
            for frame in state.resolution_stack.iter() {
                let (field, payload) = match frame {
                    ResolutionFrame::CopyToken(pending) => (
                        "pending_copy_token_resolution",
                        serde_json::to_value(pending).unwrap(),
                    ),
                    ResolutionFrame::PostReplacement(pending) => (
                        "post_replacement_drains",
                        serde_json::to_value(pending).unwrap(),
                    ),
                    ResolutionFrame::AbilityContinuation(pending) => (
                        "pending_continuation",
                        serde_json::to_value(&pending.pending).unwrap(),
                    ),
                    ResolutionFrame::CounterAdditions(pending) => (
                        "pending_counter_additions",
                        serde_json::to_value(pending).unwrap(),
                    ),
                    ResolutionFrame::SpellResolution(pending) => (
                        "pending_spell_resolution",
                        serde_json::to_value(pending).unwrap(),
                    ),
                    other => panic!(
                        "this bounded copy fixture must retain an authentic v1 carrier for {:?}",
                        other.kind()
                    ),
                };
                assert!(
                    legacy.get(field).is_none_or(serde_json::Value::is_null),
                    "v1 has exactly one owning {field}"
                );
                legacy[field] = payload;
            }
            forms.push(
                serde_json::from_value::<ResolutionStateWire>(legacy)
                    .unwrap()
                    .into_game_state(),
            );
        }
        for version in [2, 3, 4] {
            let mut wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap();
            wire["resolution_state_version"] = serde_json::json!(version);
            forms.push(
                serde_json::from_value::<ResolutionStateWire>(wire)
                    .unwrap()
                    .into_game_state(),
            );
        }
        forms
    }

    #[test]
    fn public_substituted_copy_preserves_named_host_on_both_materializers_and_counter_reload() {
        use crate::game::game_object::AttachTarget;
        use crate::game::scenario::{GameRunner, GameScenario, P0};
        for deferred in [false, true] {
            for bound in [false, true] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
                let first = scenario.add_creature(P0, "First host", 4, 4).id();
                let second = scenario.add_creature(P0, "Selected host", 5, 5).id();
                let substitution = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                    .condition(
                        crate::types::ability::ReplacementCondition::TokenSubtypeMatches {
                            subtypes: vec!["Soldier".into()],
                        },
                    )
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::CopyTokenOf {
                            target: TargetFilter::SelfRef,
                            owner: TargetFilter::Controller,
                            source_filter: None,
                            enters_attacking: false,
                            attack_destination:
                                crate::types::ability::TokenAttackDestination::ControllerChooses,
                            tapped: false,
                            count: QuantityExpr::Fixed { value: 1 },
                            extra_keywords: Vec::new(),
                            additional_modifications: if deferred {
                                vec![ContinuousModification::SetPowerDynamic {
                                    value: QuantityExpr::Fixed { value: 2 },
                                }]
                            } else {
                                Vec::new()
                            },
                        },
                    ));
                let donor = scenario
                    .add_enchantment_from_oracle(
                        P0,
                        "Synthetic copy substitution Aura",
                        "Enchant creature\nThis enchantment enters with a charge counter on it.",
                    )
                    .with_subtypes(vec!["Aura"])
                    .with_replacement_definition(substitution)
                    .id();
                let mut counter = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .quantity_modification(QuantityModification::Prevent);
                counter.mode = ReplacementMode::Optional { decline: None };
                scenario
                    .add_artifact_from_oracle(P0, "Optional counter choice", "")
                    .with_replacement_definition(counter);
                let spell = scenario
                    .add_spell_to_hand_from_oracle(P0, "Synthetic attached creation", false, "")
                    .with_mana_cost(crate::types::mana::ManaCost::generic(0))
                    .with_ability(Effect::Token {
                        name: "Seed".into(),
                        power: PtValue::Fixed(2),
                        toughness: PtValue::Fixed(2),
                        types: vec!["Creature".into(), "Soldier".into()],
                        colors: Vec::new(),
                        keywords: Vec::new(),
                        tapped: false,
                        count: QuantityExpr::Fixed { value: 1 },
                        owner: TargetFilter::Controller,
                        attach_to: Some(if bound {
                            TargetFilter::Typed(TypedFilter::creature())
                        } else {
                            TargetFilter::ParentTarget
                        }),
                        enters_attacking: false,
                        attack_destination:
                            crate::types::ability::TokenAttackDestination::ControllerChooses,
                        supertypes: Vec::new(),
                        static_abilities: Vec::new(),
                        enter_with_counters: Vec::new(),
                    })
                    .id();
                let mut runner = scenario.build();
                crate::game::effects::attach::attach_to(runner.state_mut(), donor, first);
                if bound {
                    runner
                        .cast(spell)
                        .target_object(second)
                        .stop_at_optional()
                        .resolve();
                } else {
                    runner.cast(spell).stop_at_optional().resolve();
                    assert!(runner
                        .state()
                        .objects
                        .values()
                        .all(|object| !object.is_token));
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        runner.state().stack.is_empty()
                            && runner.state().resolution_stack.is_empty()
                    );
                    continue;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P0, .. }
                ));
                assert!(matches!(
                    runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .unwrap()
                        .proposed,
                    ProposedEvent::AddCounter { count: 1, .. }
                ));
                let forms = liminal_copy_reader_forms(runner.state());
                for form in forms {
                    let mut resumed = GameRunner::from_state(form);
                    for _ in 0..8 {
                        match resumed.state().waiting_for {
                            WaitingFor::ReplacementChoice { player: P0, .. } => {
                                resumed
                                    .act(GameAction::ChooseReplacement { index: 1 })
                                    .unwrap();
                            }
                            WaitingFor::Priority { .. } => {
                                resumed.advance_until_stack_empty();
                                break;
                            }
                            ref other => panic!(
                                "named host must never become a generic host choice: {other:?}"
                            ),
                        }
                    }
                    let tokens = resumed
                        .state()
                        .battlefield
                        .iter()
                        .filter_map(|id| {
                            resumed
                                .state()
                                .objects
                                .get(id)
                                .filter(|object| object.is_token)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(tokens.len(), 1);
                    let token = tokens[0];
                    assert_eq!(token.name, "Synthetic copy substitution Aura");
                    assert_eq!(token.attached_to, Some(AttachTarget::Object(second)));
                    assert_eq!(
                        token.counters.get(&CounterType::Generic("charge".into())),
                        Some(&1)
                    );
                    assert_eq!(token.base_power, deferred.then_some(2));
                    assert!(!resumed.state().objects[&first]
                        .attachments
                        .contains(&token.id));
                    assert!(resumed.state().objects[&second]
                        .attachments
                        .contains(&token.id));
                    assert!(matches!(
                        resumed.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        resumed.state().stack.is_empty()
                            && resumed.state().resolution_stack.is_empty()
                    );
                }
            }
        }
    }

    const PRINTED_MISHRA: &str = "At the beginning of combat on your turn, create a token that's a copy of target noncreature artifact you control, except its name is Mishra's Warform and it's a 4/4 Construct artifact creature in addition to its other types. It gains haste until end of turn. Sacrifice it at the beginning of the next end step.";
    const PRINTED_CURSED_MIRROR: &str = "{T}: Add {R}.\nAs this artifact enters, you may have it become a copy of any creature on the battlefield until end of turn, except it has haste.";
    const PRINTED_GHAVE: &str = "Ghave enters with five +1/+1 counters on it.\n{1}, Remove a +1/+1 counter from a creature you control: Create a 1/1 green Saproling creature token.\n{1}, Sacrifice a creature: Put a +1/+1 counter on target creature.";

    #[test]
    fn printed_mishra_mirror_selected_entry_retains_finite_copy_counter_pause_and_cleanup() {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mishra = scenario
            .add_creature_from_oracle(P0, "Mishra, Eminent One", 5, 4, PRINTED_MISHRA)
            .id();
        let mirror = scenario
            .add_artifact_from_oracle(P0, "Cursed Mirror", PRINTED_CURSED_MIRROR)
            .id();
        let donor = scenario
            .add_creature_from_oracle(P1, "Silver Myr", 1, 1, "{T}: Add {U}.")
            .as_artifact_creature()
            .with_subtypes(vec!["Myr"])
            .id();
        for player in [P0, P1] {
            scenario.with_library_top(player, &["Reserve", "Reserve", "Reserve"]);
        }
        let mut runner = scenario.build();
        for core_type in [CoreType::Artifact, CoreType::Creature] {
            assert!(runner.state().objects[&donor]
                .card_types
                .core_types
                .contains(&core_type));
        }
        runner.advance_to_phase(Phase::BeginCombat);
        for _ in 0..20 {
            match runner.state().waiting_for.clone() {
                WaitingFor::TriggerTargetSelection { .. } => {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(mirror)),
                        })
                        .unwrap();
                }
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                _ => break,
            }
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .unwrap();
        let WaitingFor::CopyTargetChoice {
            source_id: entrant,
            purpose:
                crate::types::ability::CopyTargetPurpose::BecomeCopy {
                    continuation: Some(selected),
                },
            valid_targets,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("printed Mirror must choose before birth");
        };
        let entrant = *entrant;
        assert!(valid_targets.contains(&donor));
        assert!(
            matches!(
                selected.effect,
                Effect::BecomeCopy {
                    duration: Some(Duration::UntilEndOfTurn),
                    ..
                }
            ) || selected.duration == Some(Duration::UntilEndOfTurn)
        );
        assert!(!runner.state().objects.contains_key(&entrant));
        install_creation_fixture_replacements(runner.state_mut(), CreationPauseCase::Counter);
        let pending = runner.state().clone();
        for viewer in [Some(P0), Some(P1), None] {
            let filtered = viewer
                .map(|viewer| crate::game::visibility::filter_state_for_viewer(&pending, viewer))
                .unwrap_or_else(|| {
                    crate::game::visibility::filter_state_for_unseated_viewer(&pending)
                });
            assert!(matches!(
                filtered.waiting_for,
                WaitingFor::CopyTargetChoice {
                    purpose: CopyTargetPurpose::BecomeCopy { continuation: None },
                    ..
                }
            ));
            assert!(matches!(
                pending.waiting_for,
                WaitingFor::CopyTargetChoice {
                    purpose: CopyTargetPurpose::BecomeCopy {
                        continuation: Some(_)
                    },
                    ..
                }
            ));
        }
        let mut legacy_selected = serde_json::to_value(&pending).unwrap();
        legacy_selected["waiting_for"]["data"]["purpose"]
            .as_object_mut()
            .unwrap()
            .remove("continuation");
        let mut irrecoverable = pending.clone();
        if let WaitingFor::CopyTargetChoice {
            purpose: CopyTargetPurpose::BecomeCopy { continuation },
            ..
        } = &mut irrecoverable.waiting_for
        {
            *continuation = None;
        }
        irrecoverable
            .active_post_replacement_drains_mut()
            .unwrap()
            .resident_mut()
            .unwrap()
            .applied
            .clear();
        let invalid_wire = serde_json::to_value(&irrecoverable).unwrap();
        let unchanged = invalid_wire.clone();
        assert!(serde_json::from_value::<GameState>(invalid_wire.clone()).is_err(), "a reached legacy selected prompt with no original occurrence proof cannot invent a lifetime");
        assert_eq!(invalid_wire, unchanged);
        let recovered: GameState = serde_json::from_value(legacy_selected).unwrap();
        assert!(matches!(
            recovered.waiting_for,
            WaitingFor::CopyTargetChoice {
                purpose: CopyTargetPurpose::BecomeCopy {
                    continuation: Some(_)
                },
                ..
            }
        ));
        let mut reader_forms = liminal_copy_reader_forms(&pending);
        reader_forms.push(recovered);
        for state in reader_forms {
            let mut runner = GameRunner::from_state(state);
            let result = runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(donor)),
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, .. }
            ));
            assert!(runner.state().delayed_triggers.is_empty());
            let counter_pending = runner.state().pending_replacement.as_ref().unwrap();
            assert!(matches!(
                counter_pending.proposed,
                ProposedEvent::AddCounter { count: 1, .. }
            ));
            assert_eq!(counter_pending.candidates.len(), 2);
            let plus = counter_pending
                .candidates
                .iter()
                .position(|id| runner.state().objects[&id.source].name == "Plus counters")
                .unwrap();
            let entry_events = result.events;
            let counter_pause = runner.state().clone();
            // Replay the actual installed-copy/birth prefix from the saved pre-answer
            // state. The birth retains the original Warform recipe while the earlier
            // installed effect determines the entry snapshot's Silver Myr identity.
            let mut replay = pending.clone();
            replay.liminal_entries.clear();
            replay.pending_liminal_entry_resume = None;
            *replay.resolution_stack = Default::default();
            replay.waiting_for = WaitingFor::Priority { player: P0 };
            for entry in counter_pause
                .resolved_rules_journal
                .entries()
                .iter()
                .skip(pending.resolved_rules_journal.entries().len())
            {
                match &entry.command {
                    Some(
                        crate::types::resolved_commands::ResolvedRulesCommand::ContinuousEffect(
                            edit,
                        ),
                    ) => {
                        replay.apply_resolved_continuous_effect_edit(edit).unwrap();
                    }
                    Some(crate::types::resolved_commands::ResolvedRulesCommand::TokenCreation(
                        command,
                    )) => {
                        crate::game::effects::token::apply_resolved_token_creation(
                            &mut replay,
                            command,
                        )
                        .unwrap();
                    }
                    _ => {}
                }
            }
            let replayed = &replay.objects[&entrant];
            assert_eq!(replayed.base_display_source, Some(DisplaySource::Card));
            assert_eq!(replayed.base_token_image_ref, None);
            assert_eq!(
                (replayed.name.as_str(), replayed.base_name.as_str()),
                ("Silver Myr", "Mishra's Warform")
            );
            assert_eq!(
                replayed.layer1_copy_effect,
                counter_pause.objects[&entrant].layer1_copy_effect
            );
            assert_eq!(replay.next_object_id, counter_pause.next_object_id);
            assert_eq!(replay.zone_changes_this_turn.iter().filter(|entry| entry.object_id == entrant && entry.to_zone == Zone::Battlefield).count(), 1);
            for state in liminal_copy_reader_forms(&counter_pause) {
                let mut resumed = GameRunner::from_state(state);
                let result = resumed
                    .act(GameAction::ChooseReplacement { index: plus })
                    .unwrap();
                crate::game::layers::flush_layers(resumed.state_mut());
                let token = &resumed.state().objects[&entrant];
                assert_eq!(
                    (token.name.as_str(), token.power, token.toughness),
                    ("Silver Myr", Some(5), Some(5))
                );
                assert_eq!(
                    (
                        token.base_name.as_str(),
                        token.base_power,
                        token.base_toughness
                    ),
                    ("Mishra's Warform", Some(4), Some(4))
                );
                assert!(token.has_keyword(&Keyword::Haste));
                assert!(token.card_types.subtypes.contains(&"Myr".into()));
                let body = resumed
                    .state()
                    .zone_changes_this_turn
                    .iter()
                    .find(|record| {
                        record.object_id == entrant && record.to_zone == Zone::Battlefield
                    })
                    .unwrap();
                assert_eq!(body.name, "Silver Myr");
                assert_eq!(entry_events.iter().chain(result.events.iter()).filter(|event| matches!(event, GameEvent::TokenCreated { object_id, .. } if *object_id == entrant)).count(), 1);
                let delayed = resumed
                    .state()
                    .delayed_triggers
                    .iter()
                    .find(|trigger| trigger.source_id == mishra)
                    .unwrap();
                assert_eq!(
                    delayed.ability.target_incarnations,
                    vec![ObjectIncarnationRef::from_object(token)]
                );
                assert!(resumed.state().resolution_stack.is_empty());
                for _ in 0..100 {
                    if resumed.state().phase == Phase::End && !resumed.state().stack.is_empty() {
                        break;
                    }
                    let action = match resumed.state().waiting_for {
                        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                            attacks: vec![],
                            bands: vec![],
                        },
                        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                            assignments: vec![],
                        },
                        _ => GameAction::PassPriority,
                    };
                    resumed.act(action).unwrap();
                }
                assert_eq!(resumed.state().phase, Phase::End);
                resumed.advance_until_stack_empty();
                assert!(!resumed.state().battlefield.contains(&entrant));
                assert_eq!(resumed.state().objects[&donor].zone, Zone::Battlefield);
            }
        }
    }

    #[test]
    fn printed_mirror_cast_acquires_ghave_counters_before_sba_and_expires_to_artifact_base() {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        for (ghave, extra_static) in [(true, false), (false, false), (true, true), (false, true)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
            let mirror = scenario
                .add_artifact_to_hand_from_oracle(P0, "Cursed Mirror", PRINTED_CURSED_MIRROR)
                .id();
            let donor = if ghave {
                {
                    let id = scenario
                        .add_creature_from_oracle(P1, "Ghave, Guru of Spores", 0, 0, PRINTED_GHAVE)
                        .id();
                    scenario.with_counter(id, CounterType::Plus1Plus1, 5);
                    id
                }
            } else {
                scenario
                    .add_creature_from_oracle(P1, "Silver Myr", 1, 1, "{T}: Add {U}.")
                    .as_artifact_creature()
                    .id()
            };
            if extra_static {
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic independent entry-counter static", "")
                    .with_static_definition(
                        StaticDefinition::new(
                            crate::types::statics::StaticMode::EntersWithAdditionalCounters {
                                counter_type: CounterType::Plus1Plus1,
                                count: 1,
                            },
                        )
                        .affected(TargetFilter::Typed(
                            TypedFilter::creature().controller(ControllerRef::You),
                        )),
                    )
                    .id();
            }
            let mut runner = scenario.build();
            assert!(runner.state().objects[&donor]
                .card_types
                .core_types
                .contains(&CoreType::Creature));
            if !ghave {
                assert!(runner.state().objects[&donor]
                    .card_types
                    .core_types
                    .contains(&CoreType::Artifact));
            }
            runner.cast(mirror).commit();
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap();
            assert!(
                matches!(&runner.state().waiting_for, WaitingFor::CopyTargetChoice { valid_targets, .. } if valid_targets.contains(&donor))
            );
            eprintln!("Mirror before public target answer: ghave={ghave}; external={extra_static}; pending={:?}; liminal={:?}; frames={:?}; body={:?}; statics={:?}",
                runner.state().pending_liminal_entry_resume, runner.state().liminal_entries,
                runner.state().resolution_stack, runner.state().objects[&mirror],
                runner.state().battlefield.iter().map(|id| (*id, &runner.state().objects[id].static_definitions)).collect::<Vec<_>>());
            let work = runner.state().objects[&mirror]
                .ordinary_entry_counter_work
                .as_ref()
                .expect("modern ordinary prompt owns deferred work, even for an empty bundle");
            assert_eq!(work.external_sources.len(), usize::from(extra_static));
            assert_eq!(work.completed_dispatch, None);
            for projected in [
                crate::game::visibility::filter_state_for_viewer(runner.state(), P0),
                crate::game::visibility::filter_state_for_viewer(runner.state(), P1),
                crate::game::visibility::filter_state_for_unseated_viewer(runner.state()),
                crate::game::visibility::project_paid_cast_cleanup_authority(runner.state()),
            ] {
                assert!(projected.objects[&mirror]
                    .ordinary_entry_counter_work
                    .is_none());
                assert!(!serde_json::to_string(&projected)
                    .unwrap()
                    .contains("ordinary_entry_counter_work"));
            }
            let normalized = runner.state().normalize_for_loop();
            let normalized_again = normalized.normalize_for_loop();
            assert_eq!(
                normalized.objects[&mirror].ordinary_entry_counter_work,
                normalized_again.objects[&mirror].ordinary_entry_counter_work
            );
            if ghave && extra_static {
                for corruption in 0..3 {
                    let mut hostile = runner.state().clone();
                    let work = hostile
                        .objects
                        .get_mut(&mirror)
                        .unwrap()
                        .ordinary_entry_counter_work
                        .as_mut()
                        .unwrap();
                    match corruption {
                        0 => work.entrant.incarnation += 1,
                        1 => work.dispatch_frame.as_mut().unwrap().0 += 1,
                        2 => {
                            work.completed_dispatch =
                                Some(crate::types::game_state::PostReplacementFrameId(
                                    work.dispatch_frame.unwrap().0 + 1,
                                ))
                        }
                        _ => unreachable!(),
                    }
                    let before = serde_json::to_value(&hostile).unwrap();
                    assert!(serde_json::from_value::<GameState>(before.clone()).is_err());
                    assert!(
                        crate::types::game_state::PersistedGameState::capture(hostile.clone())
                            .into_game_state()
                            .is_err()
                    );
                    assert!(apply_as_current(
                        &mut hostile,
                        GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(donor))
                        }
                    )
                    .is_err());
                    assert_eq!(serde_json::to_value(&hostile).unwrap(), before);
                }
            }
            for mut restored in liminal_copy_reader_forms(runner.state()) {
                assert!(restored.objects[&mirror]
                    .ordinary_entry_counter_work
                    .is_some());
                apply_as_current(
                    &mut restored,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(donor)),
                    },
                )
                .unwrap();
                crate::game::layers::flush_layers(&mut restored);
                let entrant = &restored.objects[&mirror];
                assert_eq!(entrant.zone, Zone::Battlefield);
                assert_eq!(
                    entrant
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                    (if ghave { 5 } else { 0 }) + if extra_static { 1 } else { 0 }
                );
                assert_eq!(
                    entrant.power,
                    Some((if ghave { 5 } else { 1 }) + if extra_static { 1 } else { 0 })
                );
                assert!(entrant.has_keyword(&Keyword::Haste));
                assert!(entrant.ordinary_entry_counter_work.is_none());
                assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
                assert!(restored.stack.is_empty() && restored.resolution_stack.is_empty());
            }
            if !extra_static {
                // An authentic old prompt already performed its external phase.
                // With no external source that completed phase has no placements.
                let mut old = serde_json::to_value(runner.state()).unwrap();
                old["objects"][mirror.0.to_string()]
                    .as_object_mut()
                    .unwrap()
                    .remove("ordinary_entry_counter_work");
                let old: GameState = serde_json::from_value(old).unwrap();
                for mut restored in liminal_copy_reader_forms(&old) {
                    apply_as_current(
                        &mut restored,
                        GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(donor)),
                        },
                    )
                    .unwrap();
                    assert_eq!(
                        restored.objects[&mirror]
                            .counters
                            .get(&CounterType::Plus1Plus1)
                            .copied()
                            .unwrap_or(0),
                        if ghave { 5 } else { 0 }
                    );
                    assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
                    assert!(restored.resolution_stack.is_empty());
                }
            }
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(donor)),
                })
                .unwrap();
            crate::game::layers::flush_layers(runner.state_mut());
            eprintln!("Mirror after public target answer: ghave={ghave}; external={extra_static}; body={:?}; wait={:?}; frames={:?}; pending={:?}",
                runner.state().objects[&mirror], runner.state().waiting_for,
                runner.state().resolution_stack, runner.state().pending_replacement);
            assert_eq!(runner.state().objects[&mirror].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().objects[&mirror]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                (if ghave { 5 } else { 0 }) + if extra_static { 1 } else { 0 }
            );
            assert_eq!(
                runner.state().objects[&mirror].power,
                Some((if ghave { 5 } else { 1 }) + if extra_static { 1 } else { 0 })
            );
            assert!(runner.state().objects[&mirror].has_keyword(&Keyword::Haste));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().resolution_stack.is_empty());
            let mut restored =
                GameRunner::from_state(liminal_copy_reader_forms(runner.state()).remove(1));
            crate::game::turns::execute_cleanup(restored.state_mut(), &mut Vec::new());
            crate::game::layers::flush_layers(restored.state_mut());
            let object = &restored.state().objects[&mirror];
            assert_eq!(object.name, "Cursed Mirror");
            assert_eq!(
                (
                    object.base_power,
                    object.base_toughness,
                    object.power,
                    object.toughness
                ),
                (None, None, None, None)
            );
            assert_eq!(object.card_types.core_types, vec![CoreType::Artifact]);
            assert!(!object.has_keyword(&Keyword::Haste));
        }
    }

    #[test]
    fn ordinary_mirror_matches_final_body_once_and_resumes_only_counter_suffix() {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::types::ability::QuantityModification;
        for (ghave, artifact_filter, amounts, pause, expected) in [
            (true, true, vec![1], false, 5),
            (false, true, vec![1], false, 1),
            (false, true, vec![1, 2], false, 3),
            (true, false, vec![1], true, 6),
        ] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
            let mirror = scenario
                .add_artifact_to_hand_from_oracle(P0, "Cursed Mirror", PRINTED_CURSED_MIRROR)
                .id();
            let donor = if ghave {
                let id = scenario
                    .add_creature_from_oracle(P1, "Ghave, Guru of Spores", 0, 0, PRINTED_GHAVE)
                    .id();
                scenario.with_counter(id, CounterType::Plus1Plus1, 5);
                id
            } else {
                scenario
                    .add_creature_from_oracle(P1, "Silver Myr", 1, 1, "{T}: Add {U}.")
                    .as_artifact_creature()
                    .id()
            };
            for amount in amounts {
                let filter = if artifact_filter {
                    TypedFilter::new(crate::types::ability::TypeFilter::Artifact)
                } else {
                    TypedFilter::creature()
                };
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic independent entry source", "")
                    .with_static_definition(
                        StaticDefinition::new(
                            crate::types::statics::StaticMode::EntersWithAdditionalCounters {
                                counter_type: CounterType::Plus1Plus1,
                                count: amount,
                            },
                        )
                        .affected(TargetFilter::Typed(filter.controller(ControllerRef::You))),
                    );
            }
            if pause {
                let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                    .valid_card(TargetFilter::SpecificObject { id: mirror })
                    .quantity_modification(QuantityModification::Prevent);
                replacement.mode = ReplacementMode::Optional { decline: None };
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic optional entry-counter prevention", "")
                    .with_replacement_definition(replacement);
            }
            let mut runner = scenario.build();
            runner.cast(mirror).commit();
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, .. }
            ));
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap();
            assert!(
                matches!(&runner.state().waiting_for, WaitingFor::CopyTargetChoice { player: P0, valid_targets, .. } if valid_targets.contains(&donor))
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(donor)),
                })
                .unwrap();
            if pause {
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P0, .. }
                ));
                assert!(
                    runner.state().objects[&mirror]
                        .ordinary_entry_counter_work
                        .is_none(),
                    "numeric preparation was taken before its counter child"
                );
                for state in liminal_copy_reader_forms(runner.state()) {
                    let mut restored = GameRunner::from_state(state);
                    restored
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap();
                    crate::game::layers::flush_layers(restored.state_mut());
                    assert_eq!(
                        restored.state().objects[&mirror]
                            .counters
                            .get(&CounterType::Plus1Plus1)
                            .copied(),
                        Some(expected)
                    );
                    assert_eq!(
                        restored.state().objects[&mirror].power,
                        Some(expected as i32)
                    );
                    assert!(matches!(
                        restored.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert!(
                        restored.state().stack.is_empty()
                            && restored.state().resolution_stack.is_empty()
                    );
                }
            } else {
                crate::game::layers::flush_layers(runner.state_mut());
                assert_eq!(
                    runner.state().objects[&mirror]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied(),
                    Some(expected)
                );
                assert!(runner.state().objects[&mirror].has_keyword(&Keyword::Haste));
                assert!(runner.state().objects[&mirror]
                    .ordinary_entry_counter_work
                    .is_none());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty()
                );
            }
        }
    }

    #[test]
    fn ordinary_entry_external_work_completes_direct_empty_and_declined_copy() {
        use crate::game::scenario::{GameScenario, P0, P1};
        for mode in ["direct", "empty", "decline"] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
            let donor = if mode == "direct" {
                let id = scenario
                    .add_creature_from_oracle(P1, "Ghave, Guru of Spores", 0, 0, PRINTED_GHAVE)
                    .id();
                scenario.with_counter(id, CounterType::Plus1Plus1, 5);
                Some(id)
            } else {
                None
            };
            let entrant = if let Some(donor) = donor {
                scenario
                    .add_artifact_to_hand_from_oracle(P0, "Synthetic specified entry copy", "")
                    .with_replacement_definition(
                        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                            .valid_card(TargetFilter::SelfRef)
                            .destination_zone(Zone::Battlefield)
                            .execute(AbilityDefinition::new(
                                AbilityKind::Spell,
                                Effect::BecomeCopy {
                                    target: TargetFilter::SpecificObject { id: donor },
                                    recipient: crate::types::ability::CopyRecipient::Source,
                                    duration: None,
                                    mana_value_limit: None,
                                    additional_modifications: vec![],
                                },
                            )),
                    )
                    .id()
            } else {
                scenario
                    .add_artifact_to_hand_from_oracle(P0, "Cursed Mirror", PRINTED_CURSED_MIRROR)
                    .id()
            };
            scenario
                .add_artifact_from_oracle(P0, "Synthetic all-entry counter source", "")
                .with_static_definition(
                    StaticDefinition::new(
                        crate::types::statics::StaticMode::EntersWithAdditionalCounters {
                            counter_type: CounterType::Plus1Plus1,
                            count: 1,
                        },
                    )
                    .affected(TargetFilter::Any),
                );
            let mut runner = scenario.build();
            runner.cast(entrant).commit();
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            if mode != "direct" {
                assert!(
                    !runner
                        .state()
                        .battlefield
                        .iter()
                        .any(|id| runner.state().objects[id]
                            .card_types
                            .core_types
                            .contains(&CoreType::Creature)),
                    "the source population has no legal copy donor"
                );
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ) {
                    runner
                        .act(GameAction::ChooseReplacement {
                            index: if mode == "decline" { 1 } else { 0 },
                        })
                        .unwrap();
                }
            }
            assert!(!matches!(
                runner.state().waiting_for,
                WaitingFor::CopyTargetChoice { .. }
            ));
            let object = &runner.state().objects[&entrant];
            assert_eq!(object.zone, Zone::Battlefield);
            assert_eq!(
                object.counters.get(&CounterType::Plus1Plus1).copied(),
                Some(if mode == "direct" { 6 } else { 1 })
            );
            assert!(object.ordinary_entry_counter_work.is_none());
            assert_eq!(
                object.name,
                if mode == "direct" {
                    "Ghave, Guru of Spores"
                } else {
                    "Cursed Mirror"
                }
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
        }
    }

    #[test]
    fn ordinary_entry_child_draw_before_or_after_direct_copy_finishes_before_outer_counter_reader()
    {
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::types::ability::{ObjectScope, QuantityModification, QuantityRef};
        for copy_first in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
            let donor = scenario
                .add_creature_from_oracle(P1, "Ghave, Guru of Spores", 0, 0, PRINTED_GHAVE)
                .id();
            scenario.with_counter(donor, CounterType::Plus1Plus1, 5);
            let mut draw = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    target: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                },
            );
            let mut copy = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::SpecificObject { id: donor },
                    recipient: crate::types::ability::CopyRecipient::Source,
                    duration: Some(Duration::UntilEndOfTurn),
                    mana_value_limit: None,
                    additional_modifications: vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }],
                },
            );
            let selected = if copy_first {
                copy.sub_ability = Some(Box::new(draw));
                copy
            } else {
                draw.sub_ability = Some(Box::new(copy));
                draw
            };
            let entrant = scenario
                .add_artifact_to_hand_from_oracle(P0, "Synthetic original artifact", "")
                .with_replacement_definition(
                    ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                        .valid_card(TargetFilter::SelfRef)
                        .destination_zone(Zone::Battlefield)
                        .execute(selected),
                )
                .id();
            scenario
                .add_artifact_from_oracle(P0, "Synthetic creature entry bonus", "")
                .with_static_definition(
                    StaticDefinition::new(
                        crate::types::statics::StaticMode::EntersWithAdditionalCounters {
                            counter_type: CounterType::Plus1Plus1,
                            count: 1,
                        },
                    )
                    .affected(TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    )),
                );
            for modification in [
                QuantityModification::Times { factor: 2 },
                QuantityModification::Plus { value: 1 },
            ] {
                let mut replacement = ReplacementDefinition::new(ReplacementEvent::Draw)
                    .draw_scope(crate::types::ability::DrawReplacementScope::IndividualDraw);
                replacement.quantity_modification = Some(modification);
                replacement.consume_on_apply = true;
                scenario
                    .add_artifact_from_oracle(P0, "Synthetic draw replacement", "")
                    .with_replacement_definition(replacement);
            }
            let mut counter_choice = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::SpecificObject { id: entrant })
                .quantity_modification(QuantityModification::Prevent);
            counter_choice.mode = ReplacementMode::Optional { decline: None };
            scenario
                .add_artifact_from_oracle(P0, "Synthetic entrant counter prevention", "")
                .with_replacement_definition(counter_choice);
            for _ in 0..8 {
                scenario.add_card_to_library_top(P0, "Library card");
            }
            let mut outer = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SpecificObject { id: entrant },
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![(
                        CounterType::Time,
                        QuantityExpr::Fixed { value: 2 },
                    )],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            );
            outer.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::Target,
                            counter_type: Some(CounterType::Plus1Plus1),
                        },
                    },
                    player: TargetFilter::Controller,
                },
            )));
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic entry then counter reader", false)
                .with_ability_definition(outer)
                .id();
            let mut runner = scenario.build();
            let life = runner.state().players[0].life;
            runner.cast(spell).target_object(entrant).commit();
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, .. }
            ));
            assert!(matches!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .map(|pending| &pending.proposed),
                Some(crate::types::proposed_event::ProposedEvent::AddCounter { count: 2, .. })
            ));
            assert_eq!(
                runner.state().objects[&entrant]
                    .ordinary_entry_counter_work
                    .as_ref()
                    .unwrap()
                    .dispatch_frame,
                None
            );
            let initial_readers = liminal_copy_reader_forms(runner.state());
            for (reader, initial) in initial_readers.into_iter().enumerate() {
                let mut runner = GameRunner::from_state(initial);
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                assert!(runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .is_some_and(|pending| pending.candidates.len() >= 2));
                let marked = runner
                    .state()
                    .resolution_stack
                    .ability_continuations()
                    .find(|pending| pending.ordinary_entry_counter_completion.is_some())
                    .expect(
                        "the selected copy tail owns its terminal product outside the draw pair",
                    );
                if copy_first {
                    assert!(
                        matches!(marked.chain.effect, Effect::NoOp),
                        "the already-owned draw child has one completion-only ordinary tail"
                    );
                    assert_eq!(runner.state().objects[&entrant].power, Some(0));
                } else {
                    assert!(matches!(marked.chain.effect, Effect::BecomeCopy { .. }));
                    assert_eq!(runner.state().objects[&entrant].power, None);
                }
                assert_eq!(runner.state().players[0].life, life);
                let state = liminal_copy_reader_forms(runner.state()).swap_remove(reader);
                let mut restored = GameRunner::from_state(state);
                restored
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                assert!(matches!(
                    restored
                        .state()
                        .pending_replacement
                        .as_ref()
                        .map(|pending| &pending.proposed),
                    Some(crate::types::proposed_event::ProposedEvent::AddCounter { count: 6, .. })
                ));
                assert!(restored.state().objects[&entrant]
                    .ordinary_entry_counter_work
                    .is_none());
                assert_eq!(restored.state().players[0].life, life);
                let state = liminal_copy_reader_forms(restored.state()).swap_remove(reader);
                let mut restored = GameRunner::from_state(state);
                restored
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                crate::game::layers::flush_layers(restored.state_mut());
                let object = &restored.state().objects[&entrant];
                assert_eq!(object.counters.get(&CounterType::Time).copied(), Some(2));
                assert_eq!(object.zone, Zone::Battlefield);
                assert_eq!(
                    object.counters.get(&CounterType::Plus1Plus1).copied(),
                    Some(6)
                );
                assert_eq!(object.power, Some(6));
                assert!(object.has_keyword(&Keyword::Haste));
                assert!(object.ordinary_entry_counter_work.is_none());
                assert_eq!(
                    restored.state().players[0].life,
                    life + 6,
                    "the unmarked enclosing reader runs once after numeric preparation"
                );
                assert!(matches!(
                    restored.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    restored.state().stack.is_empty()
                        && restored.state().resolution_stack.is_empty()
                );
            }
        }
    }

    fn synthetic_liminal_selected_copy_prompt(
        duration: Duration,
        exceptions: Vec<ContinuousModification>,
    ) -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(1091);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        let source = creation_test_source(
            &mut state,
            "Synthetic original copy body",
            vec![CoreType::Artifact],
            vec![],
            4,
            4,
        );
        let copy = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .mode(ReplacementMode::Optional { decline: None })
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::Any,
                    recipient: crate::types::ability::CopyRecipient::Source,
                    duration: Some(duration),
                    mana_value_limit: None,
                    additional_modifications: exceptions,
                },
            ));
        let object = state.objects.get_mut(&source).unwrap();
        object.replacement_definitions.push(copy.clone());
        object.base_replacement_definitions = Arc::new(vec![copy]);
        let mut ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::SelfRef,
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
        let WaitingFor::CopyTargetChoice {
            source_id: entrant,
            purpose:
                crate::types::ability::CopyTargetPurpose::BecomeCopy {
                    continuation: Some(_),
                },
            ..
        } = state.waiting_for
        else {
            panic!("selected synthetic copy has its full captured instruction");
        };
        (state, source, entrant)
    }

    #[test]
    fn second_selected_copy_supersedes_only_owned_counter_exception_through_every_reader() {
        for accept in [false, true] {
            for matching_type in [false, true] {
                let (mut state, source, entrant) = synthetic_liminal_selected_copy_prompt(
                    Duration::Permanent,
                    vec![
                        ContinuousModification::AddCounterOnEnter {
                            counter_type: CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 2 },
                            if_type: Some(if matching_type {
                                CoreType::Creature
                            } else {
                                CoreType::Enchantment
                            }),
                        },
                        ContinuousModification::SetPowerDynamic {
                            value: QuantityExpr::Ref {
                                qty: crate::types::ability::QuantityRef::ObjectCount {
                                    filter: TargetFilter::Typed(TypedFilter::creature()),
                                },
                            },
                        },
                        ContinuousModification::SetToughnessDynamic {
                            value: QuantityExpr::Ref {
                                qty: crate::types::ability::QuantityRef::ObjectCount {
                                    filter: TargetFilter::Typed(
                                        TypedFilter::creature().properties(vec![
                                            crate::types::ability::FilterProp::Another,
                                        ]),
                                    ),
                                },
                            },
                        },
                    ],
                );
                let carrier = creation_test_source(
                    &mut state,
                    "Synthetic second copy carrier",
                    vec![CoreType::Creature],
                    vec![],
                    1,
                    1,
                );
                let parsed = crate::parser::oracle::parse_oracle_text("You may have this creature enter as a copy of any creature on the battlefield.", "Clone", &[], &["Creature".into()], &[]);
                assert_eq!(parsed.replacements.len(), 1);
                let carrier_object = state.objects.get_mut(&carrier).unwrap();
                carrier_object.replacement_definitions = parsed.replacements.clone().into();
                carrier_object.base_replacement_definitions = Arc::new(parsed.replacements);
                let final_copy = creation_test_source(
                    &mut state,
                    "Final independent Myr body",
                    vec![CoreType::Artifact, CoreType::Creature],
                    vec!["Myr".into()],
                    1,
                    1,
                );
                state
                    .liminal_entries
                    .get_mut(&entrant)
                    .unwrap()
                    .enter_with_counters
                    .push((CounterType::Generic("saved".into()), 3));
                install_test_replacement(
                    &mut state,
                    "Independent external entry counter",
                    ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                        .valid_card(TargetFilter::Any)
                        .execute(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::PutCounter {
                                target: TargetFilter::SelfRef,
                                count: QuantityExpr::Fixed { value: 1 },
                                counter_type: CounterType::Generic("external".into()),
                            },
                        )),
                );
                let static_source = creation_test_source(
                    &mut state,
                    "Independent static counter",
                    vec![CoreType::Enchantment],
                    vec![],
                    0,
                    0,
                );
                let static_definition = StaticDefinition::new(
                    crate::types::statics::StaticMode::EntersWithAdditionalCounters {
                        counter_type: CounterType::Generic("static".into()),
                        count: 2,
                    },
                )
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                let object = state.objects.get_mut(&static_source).unwrap();
                object.static_definitions.push(static_definition.clone());
                object.base_static_definitions = Arc::new(vec![static_definition]);
                // The targets were offered before these synthetic fixture objects existed.
                if let WaitingFor::CopyTargetChoice { valid_targets, .. } = &mut state.waiting_for {
                    valid_targets.push(carrier);
                }
                apply_as_current(
                    &mut state,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(carrier)),
                    },
                )
                .unwrap();
                assert!(
                    matches!(
                        state.waiting_for,
                        WaitingFor::ReplacementChoice {
                            player: PlayerId(0),
                            ..
                        }
                    ),
                    "the acquired same-slot copy replacement is a distinct occurrence"
                );
                assert!(!state.objects.contains_key(&entrant));
                let entry = &state.liminal_entries[&entrant];
                assert_eq!(entry.installed_copy_effects.len(), 1);
                let owner = entry.installed_copy_effects[0];
                let projection = entry.object.projected();
                assert_eq!(projection.layer1_copy_effect, Some(owner));
                assert!(!state.battlefield.contains(&entrant));
                // CR 614.12: the detached candidate receives the installed
                // program without joining either live quantity population.
                // Both the all-creature and recipient-relative "other" count
                // see only the carrier and final donor, never this entrant.
                assert_eq!(
                    (projection.layer_base_power, projection.layer_base_toughness),
                    (Some(2), Some(2))
                );
                for guard in 0..4 {
                    let mut foreign = state.clone();
                    let installed = foreign
                        .transient_continuous_effects
                        .iter_mut()
                        .find(|effect| effect.id == owner.continuous_effect_id)
                        .unwrap();
                    let ContinuousModification::CopyValues { values, .. } =
                        &mut installed.modifications[0]
                    else {
                        panic!("positive selected-copy path installed CopyValues");
                    };
                    values.name = "Foreign copy must not apply".into();
                    match guard {
                        0 => installed.affected = TargetFilter::SpecificObject { id: final_copy },
                        1 => {
                            installed.affected_recipient = Some(ObjectIncarnationRef::from_object(
                                &state.objects[&final_copy],
                            ))
                        }
                        2 => {
                            let mut stale = ObjectIncarnationRef::from_object(projection);
                            stale.incarnation += 1;
                            installed.affected_recipient = Some(stale);
                        }
                        3 => {
                            installed.condition = Some(crate::types::ability::StaticCondition::Or {
                                conditions: vec![],
                            })
                        }
                        _ => unreachable!(),
                    }
                    let unchanged = serde_json::to_value(&foreign).unwrap();
                    crate::game::layers::apply_installed_copy_to_liminal_entry(
                        &mut foreign,
                        entrant,
                        owner.continuous_effect_id,
                    );
                    assert_eq!(
                        serde_json::to_value(&foreign).unwrap(),
                        unchanged,
                        "guard {guard}"
                    );
                }
                assert_eq!(
                    entry
                        .copy_exception_counters
                        .as_ref()
                        .unwrap()
                        .1
                        .iter()
                        .map(|(_, count)| *count)
                        .sum::<u32>(),
                    if matching_type { 2 } else { 0 }
                );
                assert_eq!(
                    entry.enter_with_counters,
                    vec![(CounterType::Generic("saved".into()), 3)]
                );
                let selected = state.pending_replacement.as_ref().unwrap();
                assert_eq!(selected.candidate_keys.len(), 1);
                assert!(matches!(
                    selected.candidate_keys[0],
                    crate::types::proposed_event::AppliedReplacementKey::Object {
                        copy_effect: Some(_),
                        index: 0,
                        ..
                    }
                ));
                let mut old = serde_json::to_value(&state).unwrap();
                let old_entry = old["liminal_entries"][entrant.0.to_string()]
                    .as_object_mut()
                    .unwrap();
                old_entry.remove("installed_copy_effects");
                old_entry.remove("copy_exception_counters");
                let recovered: GameState = serde_json::from_value(old).unwrap();
                assert_eq!(
                    recovered.liminal_entries[&entrant].installed_copy_effects,
                    entry.installed_copy_effects
                );
                assert_eq!(
                    recovered.liminal_entries[&entrant].copy_exception_counters,
                    entry.copy_exception_counters
                );
                let mut reader_forms = liminal_copy_reader_forms(&state);
                reader_forms.push(recovered);
                for mut restored in reader_forms {
                    assert!(matches!(
                        restored.waiting_for,
                        WaitingFor::ReplacementChoice {
                            player: PlayerId(0),
                            ..
                        }
                    ));
                    apply_as_current(
                        &mut restored,
                        GameAction::ChooseReplacement {
                            index: usize::from(!accept),
                        },
                    )
                    .unwrap();
                    if accept {
                        assert!(
                            matches!(&restored.waiting_for, WaitingFor::CopyTargetChoice { valid_targets, .. } if valid_targets.contains(&final_copy))
                        );
                        apply_as_current(
                            &mut restored,
                            GameAction::ChooseTarget {
                                target: Some(TargetRef::Object(final_copy)),
                            },
                        )
                        .unwrap();
                    }
                    crate::game::layers::flush_layers(&mut restored);
                    let token = &restored.objects[&entrant];
                    assert_eq!(
                        token
                            .counters
                            .get(&CounterType::Plus1Plus1)
                            .copied()
                            .unwrap_or(0),
                        if matching_type && !accept { 2 } else { 0 }
                    );
                    for (kind, count) in [("saved", 3), ("external", 1), ("static", 2)] {
                        assert_eq!(
                            token
                                .counters
                                .get(&CounterType::Generic(kind.into()))
                                .copied(),
                            Some(count)
                        );
                    }
                    assert_eq!(token.base_name, "Synthetic original copy body");
                    assert_eq!(
                        token.name,
                        if accept {
                            "Final independent Myr body"
                        } else {
                            "Synthetic second copy carrier"
                        }
                    );
                    assert!(restored.resolution_stack.is_empty());
                    assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
                    let delayed = restored
                        .delayed_triggers
                        .iter()
                        .find(|trigger| trigger.source_id == source)
                        .unwrap();
                    assert_eq!(
                        delayed.ability.target_incarnations,
                        vec![ObjectIncarnationRef::from_object(token)]
                    );
                }
            }
        }
    }

    #[test]
    fn selected_unborn_copy_cancellation_is_exact_for_permanent_and_finite_no_host_and_prevention()
    {
        use crate::types::resolved_commands::{ResolvedContinuousEffectEdit, ResolvedRulesCommand};
        for duration in [Duration::Permanent, Duration::UntilEndOfTurn] {
            for no_host in [false, true] {
                let (mut state, source, entrant) =
                    synthetic_liminal_selected_copy_prompt(duration.clone(), vec![]);
                let donor = creation_test_source(
                    &mut state,
                    "Selected cancellation donor",
                    if no_host {
                        vec![CoreType::Enchantment]
                    } else {
                        vec![CoreType::Artifact]
                    },
                    if no_host { vec!["Aura".into()] } else { vec![] },
                    1,
                    1,
                );
                if no_host {
                    let object = state.objects.get_mut(&donor).unwrap();
                    object.keywords.push(Keyword::Enchant(TargetFilter::Typed(
                        TypedFilter::creature(),
                    )));
                    object.base_keywords = object.keywords.clone();
                    let copy = ReplacementDefinition::new(ReplacementEvent::Moved)
                        .valid_card(TargetFilter::SelfRef)
                        .destination_zone(Zone::Battlefield)
                        .mode(ReplacementMode::Optional { decline: None })
                        .execute(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::BecomeCopy {
                                target: TargetFilter::Any,
                                recipient: crate::types::ability::CopyRecipient::Source,
                                duration: None,
                                mana_value_limit: None,
                                additional_modifications: vec![],
                            },
                        ));
                    object.replacement_definitions.push(copy.clone());
                    object.base_replacement_definitions = Arc::new(vec![copy]);
                } else {
                    install_test_replacement(
                        &mut state,
                        "Later entry prevention",
                        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                            .valid_card(TargetFilter::Any)
                            .quantity_modification(QuantityModification::Prevent)
                            .mode(ReplacementMode::Optional { decline: None }),
                    );
                }
                if let WaitingFor::CopyTargetChoice { valid_targets, .. } = &mut state.waiting_for {
                    valid_targets.push(donor);
                }
                let mut replay = state.clone();
                replay.objects.remove(&entrant);
                replay.liminal_entries.clear();
                replay.pending_liminal_entry_resume = None;
                *replay.resolution_stack = Default::default();
                replay.waiting_for = WaitingFor::Priority {
                    player: PlayerId(0),
                };
                replay.next_object_id = entrant.0;
                let commands_start = state.resolved_rules_journal.entries().len();
                let independent = state.add_transient_continuous_effect(
                    entrant,
                    PlayerId(0),
                    Duration::Permanent,
                    TargetFilter::SpecificObject { id: source },
                    vec![ContinuousModification::AddPower { value: 7 }],
                    None,
                );
                apply_as_current(
                    &mut state,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(donor)),
                    },
                )
                .unwrap();
                assert!(matches!(
                    state.waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ));
                let entry = &state.liminal_entries[&entrant];
                assert_eq!(
                    entry.installed_copy_effects.len(),
                    1,
                    "positive selected installation precedes cancellation"
                );
                let pin = ObjectIncarnationRef::from_object(entry.object.projected());
                let installed = state
                    .transient_continuous_effects
                    .iter()
                    .find(|effect| {
                        effect.id == entry.installed_copy_effects[0].continuous_effect_id
                    })
                    .unwrap();
                assert_eq!(installed.duration, duration);
                assert_eq!(installed.affected_recipient, Some(pin));
                let before_cancel = state.clone();
                let result = apply_as_current(
                    &mut state,
                    GameAction::ChooseReplacement {
                        index: usize::from(no_host),
                    },
                )
                .unwrap();
                assert!(
                    !state.objects.contains_key(&entrant) && !state.battlefield.contains(&entrant)
                );
                assert!(state.liminal_entries.is_empty());
                assert!(result.events.iter().all(|event| !matches!(event, GameEvent::TokenCreated { object_id, .. } | GameEvent::ZoneChanged { object_id, .. } if *object_id == entrant)));
                assert!(state.delayed_triggers.iter().all(|trigger| trigger
                    .ability
                    .target_incarnations
                    .iter()
                    .all(|member| member.object_id != entrant)));
                assert!(state
                    .transient_continuous_effects
                    .iter()
                    .any(|effect| Some(effect.id) == independent));
                let retirement = state
                    .resolved_rules_journal
                    .entries()
                    .iter()
                    .find_map(|entry| match entry.command.as_ref() {
                        Some(ResolvedRulesCommand::ContinuousEffect(edit)) => match edit.as_ref() {
                            ResolvedContinuousEffectEdit::Retire(command)
                                if command.abandoned_recipient == Some(pin) =>
                            {
                                Some(command.clone())
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(retirement.effects.len(), 1);
                assert_eq!(
                    retirement.abandoned_next_object_id,
                    Some(state.next_object_id)
                );
                for entry in state
                    .resolved_rules_journal
                    .entries()
                    .iter()
                    .skip(commands_start)
                {
                    if let Some(ResolvedRulesCommand::ContinuousEffect(edit)) = &entry.command {
                        replay.apply_resolved_continuous_effect_edit(edit).unwrap();
                    }
                    assert!(
                        !matches!(entry.command, Some(ResolvedRulesCommand::TokenCreation(_))),
                        "install/cancel-only prefix contains no birth"
                    );
                }
                assert_eq!(replay.next_object_id, state.next_object_id);
                assert!(!replay.objects.contains_key(&entrant));
                assert!(replay
                    .transient_continuous_effects
                    .iter()
                    .any(|effect| Some(effect.id) == independent));
                for malformed in 0..5 {
                    let mut command = retirement.clone();
                    match malformed {
                        0 => {
                            command.abandoned_recipient = None;
                            command.abandoned_next_object_id = None;
                        }
                        1 => command.abandoned_next_object_id = None,
                        2 => {
                            command.abandoned_recipient =
                                Some(ObjectIncarnationRef::of(source, pin.incarnation))
                        }
                        3 => command.effects.push(command.effects[0].clone()),
                        _ => {
                            command.effects[0].affected_recipient =
                                Some(ObjectIncarnationRef::of(entrant, pin.incarnation + 1))
                        }
                    }
                    let mut invalid = before_cancel.clone();
                    let before = serde_json::to_value(&invalid).unwrap();
                    assert!(invalid
                        .apply_resolved_continuous_effect_edit(
                            &ResolvedContinuousEffectEdit::Retire(command)
                        )
                        .is_err());
                    assert_eq!(serde_json::to_value(&invalid).unwrap(), before);
                }
                let entry = &before_cancel.liminal_entries[&entrant];
                let copy = entry.copy_resume.clone().unwrap();
                let modifications = if copy.additional_modifications.is_empty() {
                    ResolvedCopyBodyModifications::NoExceptions
                } else {
                    ResolvedCopyBodyModifications::Folded {
                        modifications: copy.additional_modifications.clone(),
                        all_creature_types: before_cancel.all_creature_types.clone(),
                    }
                };
                let birth = ResolvedTokenCreationCommand {
                    object: pin,
                    owner: entry.object.projected().owner,
                    entry_timestamp: entry.object.projected().timestamp,
                    entry_turn: before_cancel.turn_number,
                    body: ResolvedTokenBody::Copy {
                        copy,
                        modifications,
                    },
                    resulting_tapped: entry.object.projected().tapped,
                    entry_choices: vec![],
                    resulting_next_object_id: before_cancel.next_object_id,
                    cause: retirement.cause,
                };
                let mut prior_birth = before_cancel.resolved_rules_journal.clone();
                prior_birth.record_token_creation(birth.clone()).unwrap();
                let unchanged = serde_json::to_value(&prior_birth).unwrap();
                assert!(prior_birth
                    .record_continuous_effect_retirement(retirement.clone())
                    .is_err());
                assert_eq!(serde_json::to_value(&prior_birth).unwrap(), unchanged);
                let mut canceled = state.resolved_rules_journal.clone();
                let unchanged = serde_json::to_value(&canceled).unwrap();
                assert!(canceled.record_token_creation(birth).is_err());
                assert_eq!(serde_json::to_value(&canceled).unwrap(), unchanged);
                let before = serde_json::to_value(&replay).unwrap();
                assert!(replay
                    .apply_resolved_continuous_effect_edit(&ResolvedContinuousEffectEdit::Retire(
                        retirement
                    ))
                    .is_err());
                assert_eq!(serde_json::to_value(&replay).unwrap(), before);
            }
        }
    }

    #[test]
    fn acquired_ghave_same_slot_enters_with_its_own_five_counters_before_sba() {
        let (mut state, _, entrant) =
            synthetic_liminal_selected_copy_prompt(Duration::Permanent, vec![]);
        let donor = creation_test_source(
            &mut state,
            "Ghave, Guru of Spores",
            vec![CoreType::Creature],
            vec![],
            0,
            0,
        );
        let parsed = crate::parser::oracle::parse_oracle_text(
            PRINTED_GHAVE,
            "Ghave, Guru of Spores",
            &[],
            &["Creature".into()],
            &[],
        );
        assert_eq!(parsed.replacements.len(), 1);
        let object = state.objects.get_mut(&donor).unwrap();
        object.replacement_definitions = parsed.replacements.clone().into();
        object.base_replacement_definitions = Arc::new(parsed.replacements);
        object.counters.insert(CounterType::Plus1Plus1, 5);
        if let WaitingFor::CopyTargetChoice { valid_targets, .. } = &mut state.waiting_for {
            valid_targets.push(donor);
        }
        let result = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(donor)),
            },
        )
        .unwrap();
        assert_eq!(state.objects[&entrant].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&entrant]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(5)
        );
        assert_eq!(
            (
                state.objects[&entrant].power,
                state.objects[&entrant].toughness
            ),
            (Some(5), Some(5))
        );
        assert_eq!(state.objects[&entrant].base_power, Some(4));
        assert_eq!(result.events.iter().filter(|event| matches!(event, GameEvent::TokenCreated { object_id, .. } if *object_id == entrant)).count(), 1);
        assert!(state.resolution_stack.is_empty());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn liminal_copy_prefix_change_keeps_resolution_occurrences_and_rejects_reinstalled_candidate() {
        use crate::types::proposed_event::{AppliedReplacementKey, ReplacementId};
        for acquired_prefix in [0, 2] {
            let (mut state, _, entrant) =
                synthetic_liminal_selected_copy_prompt(Duration::Permanent, vec![]);
            let donor = creation_test_source(
                &mut state,
                "Prefix replacement donor",
                vec![CoreType::Creature],
                vec![],
                2,
                2,
            );
            for index in 0..acquired_prefix {
                let irrelevant = ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .description(format!("Acquired prefix {index}"));
                let object = state.objects.get_mut(&donor).unwrap();
                object.replacement_definitions.push(irrelevant.clone());
                Arc::make_mut(&mut object.base_replacement_definitions).push(irrelevant);
            }
            let rider = |kind: &str, count| {
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .expiry(crate::types::ability::RestrictionExpiry::EndOfTurn)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            target: TargetFilter::SelfRef,
                            counter_type: CounterType::Generic(kind.into()),
                            count: QuantityExpr::Fixed { value: count },
                        },
                    ))
            };
            let first = rider("first already applied", 1);
            let second = rider("second eligible", 2);
            let entry = state.liminal_entries.get_mut(&entrant).unwrap();
            let mut projection = entry.object.projected().clone();
            projection.install_resolution_replacement(first).unwrap();
            projection
                .install_resolution_replacement(second.clone())
                .unwrap();
            entry.object = crate::types::game_state::LiminalEntrant::Token(
                crate::types::game_state::TokenProjection::materialize(projection),
            );
            let first = AppliedReplacementKey::for_event_in_state(
                &state,
                &ProposedEvent::TokenEntry {
                    entry_ref: entrant,
                    enter_tapped: EtbTapState::Unspecified,
                    enter_with_counters: vec![],
                    enter_with_choices: None,
                    applied: Default::default(),
                },
                ReplacementId {
                    source: entrant,
                    index: 1,
                },
            );
            // Saved-field isolation: this earlier occurrence and its already-applied
            // contribution are retained while the real public copy answer reseeds slots.
            state
                .liminal_entries
                .get_mut(&entrant)
                .unwrap()
                .enter_with_counters
                .push((CounterType::Generic("first already applied".into()), 1));
            if let Some(crate::types::game_state::PendingLiminalEntryResume::Token {
                event, ..
            }) = &mut state.pending_liminal_entry_resume
            {
                event.applied_set_mut().insert(first);
            }
            state
                .active_post_replacement_drains_mut()
                .unwrap()
                .resident_mut()
                .unwrap()
                .applied
                .insert(first);
            if let WaitingFor::CopyTargetChoice {
                valid_targets,
                purpose:
                    crate::types::ability::CopyTargetPurpose::BecomeCopy {
                        continuation: Some(selected),
                    },
                ..
            } = &mut state.waiting_for
            {
                valid_targets.push(donor);
                let mut applied = selected.replacement_applied.clone();
                applied.insert(first);
                selected.set_replacement_applied_recursive(applied);
            }
            apply_as_current(
                &mut state,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(donor)),
                },
            )
            .unwrap();
            assert!(matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            let pending = state.pending_replacement.as_ref().unwrap();
            assert_eq!(pending.candidates.len(), 1);
            assert_eq!(pending.candidates[0].index, acquired_prefix + 1);
            let second_key = pending.candidate_keys[0];
            assert!(matches!(
                second_key,
                AppliedReplacementKey::Object {
                    index: 2,
                    resolution_host: Some(_),
                    copy_effect: None,
                    ..
                }
            ));
            assert!(pending.proposed.applied_set().contains(&first));
            fn legacy_coordinates(value: &mut serde_json::Value, state: &GameState) {
                match value {
                    serde_json::Value::Object(map) => {
                        if map.contains_key("replacement_definitions") {
                            map.remove("next_resolution_replacement_installation");
                        }
                        if matches!(
                            map.get("type").and_then(serde_json::Value::as_str),
                            Some("Object" | "EntryControllerChoice")
                        ) && map.get("resolution_host").is_some()
                        {
                            let key: AppliedReplacementKey =
                                serde_json::from_value(serde_json::Value::Object(map.clone()))
                                    .unwrap();
                            let coordinate =
                                crate::game::replacement::replacement_id_for_key(state, key)
                                    .unwrap();
                            map.insert("index".into(), serde_json::json!(coordinate.index));
                            map.remove("resolution_host");
                        }
                        if map
                            .get("origin")
                            .is_some_and(|origin| origin.get("Resolution").is_some())
                        {
                            map.insert("origin".into(), serde_json::json!("Resolution"));
                        }
                        for child in map.values_mut() {
                            legacy_coordinates(child, state);
                        }
                    }
                    serde_json::Value::Array(values) => {
                        for child in values {
                            legacy_coordinates(child, state);
                        }
                    }
                    _ => {}
                }
            }
            let mut old = serde_json::to_value(&state).unwrap();
            legacy_coordinates(&mut old, &state);
            old["pending_replacement"]
                .as_object_mut()
                .unwrap()
                .remove("candidate_keys");
            let recovered: GameState = serde_json::from_value(old).unwrap();
            assert_eq!(
                recovered
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .candidate_keys,
                pending.candidate_keys
            );
            assert_eq!(
                recovered.liminal_entries[&entrant]
                    .object
                    .projected()
                    .next_resolution_replacement_installation,
                3
            );
            let mut reader_forms = liminal_copy_reader_forms(&state);
            reader_forms.push(recovered);
            for mut restored in reader_forms {
                let mut stale = restored.clone();
                let entry = stale.liminal_entries.get_mut(&entrant).unwrap();
                let mut projection = entry.object.projected().clone();
                projection.replacement_definitions.retain(|definition| {
                    !matches!(
                        definition.origin,
                        crate::types::ability::ReplacementOrigin::Resolution {
                            installation: Some(2)
                        }
                    )
                });
                projection
                    .install_resolution_replacement(second.clone())
                    .unwrap();
                assert_eq!(projection.next_resolution_replacement_installation, 4);
                entry.object = crate::types::game_state::LiminalEntrant::Token(
                    crate::types::game_state::TokenProjection::materialize(projection),
                );
                assert!(
                    crate::game::replacement::replacement_id_for_key(&stale, second_key).is_none()
                );
                let before = serde_json::to_value(&stale).unwrap();
                assert!(
                    apply_as_current(&mut stale, GameAction::ChooseReplacement { index: 0 })
                        .is_err()
                );
                assert_eq!(serde_json::to_value(&stale).unwrap(), before);
                apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 0 })
                    .unwrap();
                let token = &restored.objects[&entrant];
                assert_eq!(
                    token
                        .counters
                        .get(&CounterType::Generic("first already applied".into()))
                        .copied(),
                    Some(1)
                );
                assert_eq!(
                    token
                        .counters
                        .get(&CounterType::Generic("second eligible".into()))
                        .copied(),
                    Some(2)
                );
                assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
                assert!(restored.resolution_stack.is_empty());
            }
        }
    }

    #[test]
    fn current_liminal_copy_counter_emit_keeps_exact_creation_owner_through_reload() {
        for plus_first in [true, false] {
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
            let effect = crate::parser::oracle_effect::try_parse_token(
                &oracle.to_lowercase(),
                oracle,
                &mut ctx,
            )
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

            let copied_entry = apply_as_current(
                &mut state,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target_id)),
                },
            )
            .expect("copy target choice should resolve");
            events.extend(copied_entry.events);
            assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ), "liminal copied entry must reach counters: wait={:?}; pending={:?}; frames={:?}; events={events:?}",
            state.waiting_for, state.pending_replacement, state.resolution_stack);
            assert!(state.delayed_triggers.is_empty());
            let pending = state
                .pending_replacement
                .as_ref()
                .expect("the selected effective creature reaches its counter event");
            assert!(
                matches!(pending.proposed, ProposedEvent::AddCounter { count: 1, placement: crate::types::proposed_event::CounterPlacement::Object { object_id, actor: PlayerId(0), .. }, .. } if object_id == liminal_id)
            );
            assert_eq!(pending.candidates.len(), 2);
            let named = if plus_first {
                "Plus counters"
            } else {
                "Double counters"
            };
            let selected = pending
                .candidates
                .iter()
                .position(|id| state.objects[&id.source].name == named)
                .unwrap();
            assert!(pending
                .candidates
                .iter()
                .any(|id| state.objects[&id.source].name == "Plus counters"));
            assert!(pending
                .candidates
                .iter()
                .any(|id| state.objects[&id.source].name == "Double counters"));
            for round in 0..30 {
                if state.resolution_stack.is_empty() {
                    break;
                }
                state = serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
                )
                .unwrap()
                .into_game_state();
                events.extend(
                    apply_as_current(
                        &mut state,
                        GameAction::ChooseReplacement {
                            index: if round == 0 { selected } else { 0 },
                        },
                    )
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
            assert_eq!(
                state.objects[&liminal_id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                if plus_first { 4 } else { 3 }
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                    .count(),
                1
            );
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { record, .. } if record.object_id == liminal_id && record.to_zone == Zone::Battlefield)).count(), 1);
            assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));

            let token = state
                .objects
                .get(&liminal_id)
                .expect("liminal token should commit after choice");
            assert_eq!(token.name, "Silver Myr");
            assert_eq!(token.power, Some(if plus_first { 5 } else { 4 }));
            assert_eq!(token.toughness, token.power);
            assert_eq!(token.base_name, "Mishra's Warform");
            assert_eq!(token.base_power, Some(4));
            assert_eq!(token.base_toughness, Some(4));
            assert!(token.card_types.core_types.contains(&CoreType::Artifact));
            assert!(token.card_types.core_types.contains(&CoreType::Creature));
            assert!(token.card_types.subtypes.contains(&"Myr".to_string()));
            assert!(token
                .base_card_types
                .subtypes
                .contains(&"Construct".to_string()));
        }
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
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
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
        )
        .expect("token creation fixture admits its complete program");
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
            let pending = state.pending_replacement.as_ref().unwrap();
            assert!(
                matches!(pending.proposed, ProposedEvent::AddCounter { count: 1, .. }),
                "only the noncommuting counter modifiers require an order choice"
            );
            assert_eq!(pending.candidates.len(), 2);
            let plus = pending
                .candidates
                .iter()
                .position(|replacement| state.objects[&replacement.source].name == "Plus counters")
                .expect("the +1 modifier is an actual offered replacement");
            assert!(pending
                .candidates
                .iter()
                .any(|replacement| state.objects[&replacement.source].name == "Double counters"));
            prompts += 1;
            state = serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(state)).unwrap(),
            )
            .unwrap()
            .into_game_state();
            events.extend(
                apply_as_current(&mut state, GameAction::ChooseReplacement { index: plus })
                    .unwrap()
                    .events,
            );
        }
        assert_eq!(prompts, 4, "one noncommuting counter order per birth; the Divine/body and token-count replacements commute");
        assert!(state.resolution_stack.is_empty());
        assert_eq!(state.delayed_triggers.len(), 1);
        assert!(state.delayed_triggers[0].one_shot);
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
            assert_eq!(
                object
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                4,
                "the chosen order is (one + one) doubled for each exact birth"
            );
        }
        let unchanged_source = serde_json::to_value(&state.objects[&source]).unwrap();
        let mut runner = crate::game::scenario::GameRunner::from_state(state);
        for _ in 0..150 {
            if !runner.state().stack.is_empty() {
                break;
            }
            // CR 508.1 + CR 509.1: answer the actual combat action while
            // advancing to the independently asserted delayed trigger.
            let action = match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                _ => GameAction::PassPriority,
            };
            events.extend(runner.act(action).unwrap().events);
        }
        assert!(!runner.state().stack.is_empty());
        let delayed_entry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == source)
            .unwrap();
        assert_eq!(delayed_entry.ability().unwrap().target_incarnations, births);
        for _ in 0..40 {
            if runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        for member in &births {
            // CR 701.21a: prove the actual exact-birth sacrifice before SBA absence.
            assert_eq!(events.iter().filter(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, record }
                if *object_id == member.object_id && record.trigger_source_context.as_ref().is_some_and(|context| context.identity.reference == *member)
            )).count(), 1);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event,
                        GameEvent::PermanentSacrificed { object_id, player_id: PlayerId(0) }
                        if *object_id == member.object_id
                    ))
                    .count(),
                1
            );
        }
        for member in &births {
            // CR 111.7 + CR 704.5d: the sacrificed tokens cease to exist.
            assert!(!member.is_current(runner.state()));
            assert!(!runner.state().objects.contains_key(&member.object_id));
            assert!(!runner.state().battlefield.contains(&member.object_id));
        }
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(runner.state().return_result_frames.is_empty());
        assert!(runner.state().active_return_result_occurrence.is_none());
        assert!(runner.state().resolving_stack_entry.is_none());
        assert!(runner.state().pending_resolution_completion.is_none());
        assert!(runner.state().pending_liminal_entry_resume.is_none());
        assert!(runner.state().liminal_entries.is_empty());
        assert!(runner.state().pending_replacement.is_none());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        // CR 603.7b: the delayed instruction fires only once.
        assert!(runner.state().delayed_triggers.is_empty());
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(
            serde_json::to_value(&runner.state().objects[&source]).unwrap(),
            unchanged_source
        );
    }

    // ── Chatterfang's appended Squirrels share the created referent ──────────
    //
    // Chatterfang ruling: the additional Squirrels "won't have any abilities the
    // other tokens were created with", but anything else the creating effect
    // specifies ("That token gains haste", "Exile that token …") applies to both
    // the original tokens and the Squirrels.

    const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";
    const COGWORK_ASSEMBLER: &str = "{7}: Create a token that's a copy of target artifact. That token gains haste. Exile it at the beginning of the next end step.";
    const HEAT_SHIMMER: &str = "Create a token that's a copy of target creature, except it has haste and \"At the beginning of the end step, exile this token.\"";

    /// Every battlefield token named `name`.
    fn battlefield_tokens(state: &GameState, name: &str) -> Vec<ObjectId> {
        state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects[id].is_token && state.objects[id].name == name)
            .collect()
    }

    /// The one battlefield token named `name`.
    fn the_battlefield_token(state: &GameState, name: &str) -> ObjectId {
        let found = battlefield_tokens(state, name);
        assert_eq!(found.len(), 1, "exactly one {name} token: {found:?}");
        found[0]
    }

    /// Advance to the end step with no attackers and let its triggers resolve.
    fn finish_at_end_step(runner: &mut crate::game::scenario::GameRunner) {
        use crate::types::phase::Phase;

        runner.advance_to_end_step();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .declare_attackers(&[])
                .expect("declaring no attackers is legal");
            runner.advance_to_end_step();
        }
        assert_eq!(runner.state().phase, Phase::End);
        runner.advance_until_stack_empty();
    }

    /// CR 707.2 + CR 614.1a + CR 608.2c: Cogwork Assembler with Chatterfang out,
    /// on the copy route: one copy and one Squirrel, both hasty and both in the
    /// created referent (the copy first); at the next end step "Exile it" exiles
    /// both.
    #[test]
    fn chatterfang_squirrel_shares_copy_token_exile_rider() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let assembler = scenario
            .add_artifact_from_oracle(P0, "Cogwork Assembler", COGWORK_ASSEMBLER)
            .id();
        scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
        scenario.with_mana_pool(
            P0,
            (0..7)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        let index = runner.state().objects[&assembler]
            .abilities
            .iter()
            .position(|ability| matches!(ability.kind, AbilityKind::Activated))
            .expect("Cogwork Assembler's activated ability");
        runner
            .activate(assembler, index)
            .target_object(assembler)
            .resolve();
        assert!(runner.state().stack.is_empty(), "the activation resolved");

        let state = runner.state();
        // Reach-guards: the copy exists and Chatterfang's replacement applied.
        let copy = the_battlefield_token(state, "Cogwork Assembler");
        let squirrel = the_battlefield_token(state, "Squirrel");
        assert!(state.objects[&copy].has_keyword(&Keyword::Haste));
        // CR 702.10a: "That token gains haste" reaches the Squirrel too.
        assert!(
            state.objects[&squirrel].has_keyword(&Keyword::Haste),
            "the Squirrel gains haste"
        );
        assert_eq!(state.last_created_token_ids, vec![copy, squirrel]);
        assert!(
            state.resolution_stack.is_empty(),
            "the exact compound creation owner and its rider both completed"
        );
        let delayed = state
            .delayed_triggers
            .iter()
            .find(|trigger| trigger.source_id == assembler)
            .expect("the compound Cogwork instruction registered its next-end exile");
        assert_eq!(
            delayed.condition,
            crate::types::ability::DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            "Cogwork's exact next-end condition: {delayed:?}"
        );
        assert!(delayed.one_shot);
        assert_eq!(
            delayed.ability.target_incarnations,
            [copy, squirrel]
                .map(|id| ObjectIncarnationRef::from_object(&state.objects[&id]))
                .to_vec(),
            "the delayed exile retains every exact created birth: {delayed:?}"
        );
        let births = delayed.ability.target_incarnations.clone();
        let registered = delayed.clone();

        // CR 603.7c: the delayed "Exile it" affects both tokens.
        let mut events = Vec::new();
        for _ in 0..160 {
            if runner.state().stack.iter().any(|entry| {
                entry.source_id == assembler
                    && entry
                        .ability()
                        .is_some_and(|ability| ability.target_incarnations == births)
            }) {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                _ => GameAction::PassPriority,
            };
            events.extend(runner.act(action).unwrap().events);
        }
        assert_eq!(runner.state().phase, Phase::End,
            "Cogwork observation: registered={registered:?}; births={births:?}; stack={:?}; delayed={:?}; wait={:?}; events={events:?}",
            runner.state().stack, runner.state().delayed_triggers, runner.state().waiting_for);
        assert!(runner.state().stack.iter().any(|entry| entry.source_id == assembler
            && entry.ability().is_some_and(|ability| ability.target_incarnations == births)),
            "the registered Cogwork delayed trigger reached the stack: phase={:?}; stack={:?}; registered={registered:?}; births={births:?}; delayed={:?}; wait={:?}; events={events:?}",
            runner.state().phase, runner.state().stack, runner.state().delayed_triggers, runner.state().waiting_for);
        for _ in 0..40 {
            if runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        let state = runner.state();
        assert!(!state.battlefield.contains(&copy), "the copy is exiled");
        assert!(
            !state.battlefield.contains(&squirrel),
            "the Squirrel is exiled"
        );
        for member in &births {
            assert_eq!(events.iter().filter(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Exile, record }
                if *object_id == member.object_id && record.trigger_source_context.as_ref().is_some_and(|context| context.identity.reference == *member)
            )).count(), 1);
        }
        // CR 603.7b: both exact births left once and the one-shot is consumed.
        assert!(state
            .delayed_triggers
            .iter()
            .all(|trigger| trigger.source_id != assembler));
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        assert!(state.return_result_frames.is_empty());
        assert!(state.active_return_result_occurrence.is_none());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.pending_resolution_completion.is_none());
        assert!(state.pending_liminal_entry_resume.is_none());
        assert!(state.liminal_entries.is_empty());
        assert!(state.pending_replacement.is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// Negative sibling (Chatterfang ruling: the Squirrels "won't have any
    /// abilities the other tokens were created with"): Heat Shimmer's haste and
    /// end-step exile are copy exceptions (CR 707.9a), so they belong to the copy
    /// only. The Squirrel is not hasty and survives the end step.
    #[test]
    fn heat_shimmer_copy_exceptions_do_not_reach_chatterfang_squirrel() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let shimmer = scenario
            .add_spell_to_hand_from_oracle(P0, "Heat Shimmer", false, HEAT_SHIMMER)
            .id();
        let mut runner = scenario.build();
        runner.cast(shimmer).target_object(bears).resolve();

        let state = runner.state();
        let copy = the_battlefield_token(state, "Grizzly Bears");
        let squirrel = the_battlefield_token(state, "Squirrel");
        assert!(state.objects[&copy].has_keyword(&Keyword::Haste));
        assert!(
            !state.objects[&squirrel].has_keyword(&Keyword::Haste),
            "the Squirrel does not get the copy's haste"
        );

        finish_at_end_step(&mut runner);
        let state = runner.state();
        // Reach-guard: the copy's own end-step trigger exiled it.
        assert!(!state.battlefield.contains(&copy), "the copy is exiled");
        assert!(
            state.battlefield.contains(&squirrel),
            "the Squirrel survives the end step"
        );
    }
}

//! CR 101.4 + CR 707.2 + CR 122.1: `Effect::EachPlayerCopyChosen`.
//!
//! Each player, in APNAP order, chooses an ordered `min..=max` selection of
//! objects they control matching `choose_filter`; creates a token copy of the
//! first chosen (with `copy_modifications`); then, if `scale` is set and a
//! second object was chosen, puts `scale.counter_type` counters on the created
//! token equal to `scale.scale_property` of the second chosen object (read live
//! at placement, CR 122.1).
//!
//! This is a self-iterating effect (excluded from `player_scope` fan-out in
//! `resolve_ability_chain`, mirroring [`super::choose_and_sacrifice_rest`]). It
//! walks the scoped player set itself and seeds
//! `WaitingFor::EachPlayerCopyChosenSelection` per player.
//!
//! Unlike `ChooseAndSacrificeRest`, the per-player step is a genuine deferred
//! continuation: the inner `CopyTokenOf` can pause on a CR 616.1 replacement
//! choice (`CopyToken`), and the counter placement can pause
//! on a competing counter-replacement ordering (`CounterAdditions`).
//! Both pauses are handled by parking a [`PendingEachPlayerCopyChosen`] record
//! with a [`CopyChosenStage`] marker and resuming from `drain_pending`, which
//! `engine_replacement.rs` invokes after those primitive drains once state is
//! back at Priority.

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::players;
use crate::game::quantity::aggregate_property_over;
use crate::types::ability::{
    AggregateFunction, ContinuousModification, CopyChooseScope, CopyScale, Effect, EffectError,
    EffectKind, PlayerFilter, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CopyChosenSelection, CopyChosenStage, GameState, PendingEachPlayerCopyChosen, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Effect parameters threaded through the whole APNAP walk. Bundled so the
/// resolver, the `SelectTargets` continuation, and the replacement-resume drain
/// all reconstruct the same state.
#[derive(Clone)]
pub(crate) struct CopyChosenParams {
    pub choose_filter: TargetFilter,
    pub min: u32,
    pub max: u32,
    pub copy_modifications: Vec<ContinuousModification>,
    pub scale: Option<CopyScale>,
    /// CR 102.1 + CR 103.1: whose battlefield each chooser draws eligible objects
    /// from, relative to the chooser.
    pub choose_scope: CopyChooseScope,
    pub source_id: ObjectId,
    pub source_controller: PlayerId,
    pub scoped_players: Vec<PlayerId>,
    pub trigger_event: Option<GameEvent>,
}

impl CopyChosenParams {
    pub(crate) fn from_pending(p: &PendingEachPlayerCopyChosen) -> Self {
        Self {
            choose_filter: p.choose_filter.clone(),
            min: p.min,
            max: p.max,
            copy_modifications: p.copy_modifications.clone(),
            scale: p.scale.clone(),
            choose_scope: p.choose_scope,
            source_id: p.source_id,
            source_controller: p.source_controller,
            scoped_players: p.scoped_players.clone(),
            trigger_event: p.trigger_event.clone(),
        }
    }
}

/// CR 101.4: Entry point — establish the APNAP-scoped player order and begin the
/// walk. Dispatched from `resolve_effect`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (choose_filter, min, max, copy_modifications, scale, choose_scope) = match &ability.effect {
        Effect::EachPlayerCopyChosen {
            choose_filter,
            min,
            max,
            copy_modifications,
            scale,
            choose_scope,
        } => (
            choose_filter.clone(),
            *min,
            *max,
            copy_modifications.clone(),
            scale.clone(),
            *choose_scope,
        ),
        _ => {
            return Err(EffectError::MissingParam(
                "EachPlayerCopyChosen".to_string(),
            ))
        }
    };

    // CR 101.4 + CR 608.2c: the scope. "Each player" is `All`; a `player_scope`
    // set to Opponent/etc. restricts the walk to the scoped players (mirrors
    // `choose_and_sacrifice_rest::resolve`).
    let scope = ability.player_scope.clone().unwrap_or(PlayerFilter::All);
    let player_order: Vec<PlayerId> = players::apnap_order(state)
        .into_iter()
        .filter(|pid| {
            super::matches_player_scope(state, *pid, &scope, ability.controller, ability.source_id)
        })
        .collect();

    let params = CopyChosenParams {
        choose_filter,
        min,
        max,
        copy_modifications,
        scale,
        choose_scope,
        source_id: ability.source_id,
        source_controller: ability.controller,
        scoped_players: player_order.clone(),
        // CR 608.2: preserve the phenomenon trigger's event across the walk.
        trigger_event: state.current_trigger_event.clone(),
    };

    // Walk the whole ordered set; `advance_to_next_player` collects choices first
    // and defers all copy/counter actions until the choice set is complete.
    advance_to_next_player(state, player_order, Vec::new(), &params, events)
}

/// CR 101.3: Advance to the next player in APNAP order. Skips players with no
/// eligible object (CR 101.3, nothing to do), auto-resolves a forced single (an
/// engine UX optimization, not a CR-specified rule), or seeds the `WaitingFor`
/// selection. When `players_left` is exhausted, emits the terminal
/// `EffectResolved` (the callers own the Priority sentinel).
pub(crate) fn advance_to_next_player(
    state: &mut GameState,
    players_left: Vec<PlayerId>,
    all_choices: Vec<CopyChosenSelection>,
    params: &CopyChosenParams,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut players_left = players_left;
    let mut all_choices = all_choices;
    loop {
        if players_left.is_empty() {
            return drive_choices(state, all_choices, params, events);
        }
        let player = players_left[0];
        let rest: Vec<PlayerId> = players_left[1..].to_vec();

        let ctx =
            FilterContext::from_source_with_controller(params.source_id, params.source_controller);
        let eligible = compute_eligible(
            state,
            player,
            &params.choose_filter,
            params.choose_scope,
            &ctx,
        );

        // CR 101.3: a player with no eligible object does nothing — skip.
        if eligible.is_empty() {
            players_left = rest;
            continue;
        }

        // A forced single (exactly one eligible object and `min == 1`) has only
        // one legal selection — record it without prompting. This is an engine UX
        // optimization, not a CR-specified rule; the action still waits until all
        // CR 101.4 APNAP choices have been collected.
        if eligible.len() == 1 && params.min <= 1 {
            all_choices.push(CopyChosenSelection {
                player,
                chosen: vec![eligible[0]],
            });
            players_left = rest;
            continue;
        }

        state.waiting_for = WaitingFor::EachPlayerCopyChosenSelection {
            player,
            eligible: eligible.into_iter().map(TargetRef::Object).collect(),
            min: params.min,
            max: params.max,
            choose_filter: params.choose_filter.clone(),
            copy_modifications: params.copy_modifications.clone(),
            scale: params.scale.clone(),
            choose_scope: params.choose_scope,
            source_id: params.source_id,
            source_controller: params.source_controller,
            remaining_players: rest,
            all_choices,
            scoped_players: params.scoped_players.clone(),
            trigger_event: params.trigger_event.clone(),
        };
        return Ok(());
    }
}

/// CR 101.4 + CR 707.2: Once every player has made their APNAP choice, perform
/// the copy/counter actions from the completed choice set.
pub(crate) fn drive_choices(
    state: &mut GameState,
    choices: Vec<CopyChosenSelection>,
    params: &CopyChosenParams,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some((current, rest)) = choices.split_first() else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::EachPlayerCopyChosen,
            source_id: params.source_id,
            subject: None,
        });
        return Ok(());
    };
    drive_from_copy(
        state,
        current.player,
        current.chosen.clone(),
        rest.to_vec(),
        params,
        events,
    )
}

/// CR 707.2 + CR 616.1: Copy the first chosen object for `player`, then drive the
/// counter step. Detects a CR 616.1 pause of the inner copy (parking
/// `CopyToken`) and, rather than trusting the `Ok(())` from
/// `resolve_ability_chain`, parks an `AwaitingCopy` continuation and preserves
/// the copy's replacement `WaitingFor`.
pub(crate) fn drive_from_copy(
    state: &mut GameState,
    player: PlayerId,
    chosen: Vec<ObjectId>,
    remaining_choices: Vec<CopyChosenSelection>,
    params: &CopyChosenParams,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 707.2 + CR 205.4: create a token copy of `chosen[0]` under `player`'s
    // control, with the "except …" modifications applied.
    let copy_source = match chosen.first() {
        Some(id) => *id,
        None => return drive_choices(state, remaining_choices, params, events),
    };
    let copy_ability = ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: crate::types::ability::default_target_filter_any(),
            owner: TargetFilter::Controller,
            source_filter: Some(TargetFilter::SpecificObject { id: copy_source }),
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: params.copy_modifications.clone(),
        },
        vec![],
        params.source_id,
        player,
    );
    // Depth 1: skip the depth-0 chain prelude so per-resolution ledgers
    // (`last_created_token_ids`) are not reset.
    let stack_depth_before_copy = state.resolution_stack.len();
    super::resolve_ability_chain(state, &copy_ability, events, 1)?;

    // CR 616.1: The copy parked a replacement-ordering choice. Do NOT read
    // `last_created_token_ids` (stale) and do NOT advance (that would clobber the
    // replacement `WaitingFor`). Park an `AwaitingCopy` continuation.
    if state.active_copy_token().is_some() {
        park_each_player_copy_chosen_after_current_step(
            state,
            make_pending(
                CopyChosenStage::AwaitingCopy,
                player,
                chosen,
                remaining_choices,
                params,
            ),
            stack_depth_before_copy,
        );
        return Ok(());
    }

    perform_counter_step_then_advance(state, player, chosen, remaining_choices, params, events)
}

/// CR 122.1 + CR 208.1: Place the scaling counters (if any) on the created
/// token(s), then advance. Pause-aware: if the counter placement parks a
/// competing counter-replacement ordering (`CounterAdditions`), park an
/// `AwaitingCounters` continuation instead of advancing.
pub(crate) fn perform_counter_step_then_advance(
    state: &mut GameState,
    player: PlayerId,
    chosen: Vec<ObjectId>,
    remaining_choices: Vec<CopyChosenSelection>,
    params: &CopyChosenParams,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // All ids created by the copy (N>1 under a token doubler).
    let created_nonempty = !state.last_created_token_ids.is_empty();
    if let Some(scale) = &params.scale {
        if chosen.len() == 2 && created_nonempty {
            // CR 122.1 + CR 208.1: read the second chosen object's property live
            // at placement (LKI fallback if it has left the battlefield → 0).
            let amount = aggregate_property_over(
                state,
                &[chosen[1]],
                AggregateFunction::Max,
                scale.scale_property,
            )
            .max(0);
            if amount > 0 {
                // Target `LastCreated` so counters land on EVERY created copy
                // (the full `last_created_token_ids` vector); `resolve_add`
                // self-manages any per-object counter-replacement pause.
                let counter_ability = ResolvedAbility::new(
                    Effect::PutCounter {
                        counter_type: scale.counter_type.clone(),
                        count: QuantityExpr::Fixed { value: amount },
                        target: TargetFilter::LastCreated,
                    },
                    vec![],
                    params.source_id,
                    player,
                );
                let stack_depth_before_counter = state.resolution_stack.len();
                super::resolve_ability_chain(state, &counter_ability, events, 1)?;
                // CR 616.1: the counter placement paused for a replacement
                // ordering — park an `AwaitingCounters` continuation.
                if state.active_counter_additions().is_some() {
                    park_each_player_copy_chosen_after_current_step(
                        state,
                        make_pending(
                            CopyChosenStage::AwaitingCounters,
                            player,
                            chosen,
                            remaining_choices,
                            params,
                        ),
                        stack_depth_before_counter,
                    );
                    return Ok(());
                }
            }
        }
    }
    drive_choices(state, remaining_choices, params, events)
}

/// CR 616.1: Resume the walk after a copy or counter pause. Invoked from both
/// replacement-resume arms in `engine_replacement.rs` once the paused primitive
/// has fully drained and state is back at Priority.
pub(crate) fn drain_pending(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // Guard invariants: never resume while a primitive of the current step is
    // still mid-flight (a copy re-paused under a second doubler, or a counter
    // ordering is still open).
    if state.active_copy_token().is_some() || state.active_counter_additions().is_some() {
        return;
    }
    let Some(pending) = state
        .take_active_each_player_copy_chosen()
        .expect("each-player-copy-chosen drain may consume only the active frame")
    else {
        return;
    };
    let params = CopyChosenParams::from_pending(&pending);
    let result = match pending.stage {
        // The copy just finished draining; `last_created_token_ids` is populated
        // — drive the counter step.
        CopyChosenStage::AwaitingCopy => perform_counter_step_then_advance(
            state,
            pending.player,
            pending.chosen,
            pending.remaining_choices,
            &params,
            events,
        ),
        // The counter placement finished draining — continue the collected action walk.
        CopyChosenStage::AwaitingCounters => {
            drive_choices(state, pending.remaining_choices, &params, events)
        }
    };
    // The walk is infallible in practice (copy/counter no-ops on impossible
    // parts, CR 101.3); a stray error just ends this effect's resume cleanly.
    debug_assert!(
        result.is_ok(),
        "each_player_copy_chosen drain error: {result:?}"
    );
    let _ = result;
}

/// Park the outer APNAP copy walk below the complete child stack raised by its
/// current copy or counter step. The captured boundary preserves the exact
/// parent/child dependency without searching for a buried continuation.
fn park_each_player_copy_chosen_after_current_step(
    state: &mut GameState,
    pending: PendingEachPlayerCopyChosen,
    stack_depth_before_step: usize,
) {
    match state.resolution_stack.len().cmp(&stack_depth_before_step) {
        std::cmp::Ordering::Less => {
            panic!("each-player-copy-chosen step removed a parent before it could be re-parked")
        }
        std::cmp::Ordering::Equal => state.push_each_player_copy_chosen(pending),
        std::cmp::Ordering::Greater => state
            .insert_each_player_copy_chosen_parent_at_child_boundary(
                pending,
                stack_depth_before_step,
            )
            .expect("each-player-copy-chosen parent must be inserted below its child stack"),
    }
}

/// CR 102.1 + CR 103.1: The battlefield controller a chooser draws their
/// eligible pool from, given the effect's `choose_scope`. `Chooser` = the
/// chooser themselves; `Neighbor { direction }` = the seat-neighbor resolved by
/// the `players::neighbor` authority.
fn pool_controller(state: &GameState, chooser: PlayerId, scope: CopyChooseScope) -> PlayerId {
    match scope {
        CopyChooseScope::Chooser => chooser,
        CopyChooseScope::Neighbor { direction } => players::neighbor(state, chooser, direction),
    }
}

/// Compute the objects matching `choose_filter` on the battlefield of the
/// controller `scope` designates relative to `player` (their own or a neighbor's).
fn compute_eligible(
    state: &GameState,
    player: PlayerId,
    choose_filter: &TargetFilter,
    scope: CopyChooseScope,
    ctx: &FilterContext<'_>,
) -> Vec<ObjectId> {
    let controller = pool_controller(state, player, scope);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == controller
                    && !obj.is_emblem
                    && matches_target_filter(state, *id, choose_filter, ctx)
            })
        })
        .collect()
}

/// CR 608.2c: A submitted choice must still be legal when the response resolves;
/// prompt snapshots are not authority if the board changed before submission.
pub(crate) fn is_live_eligible_choice(
    state: &GameState,
    player: PlayerId,
    id: ObjectId,
    choose_filter: &TargetFilter,
    scope: CopyChooseScope,
    source_id: ObjectId,
    source_controller: PlayerId,
) -> bool {
    let ctx = FilterContext::from_source_with_controller(source_id, source_controller);
    // CR 102.1 + CR 103.1: re-resolve the eligibility controller live so a
    // seat-relative pool tracks any control change since the prompt was seeded.
    let controller = pool_controller(state, player, scope);
    state.objects.get(&id).is_some_and(|obj| {
        obj.controller == controller
            && !obj.is_emblem
            && state.battlefield.contains(&id)
            && matches_target_filter(state, id, choose_filter, &ctx)
    })
}

fn make_pending(
    stage: CopyChosenStage,
    player: PlayerId,
    chosen: Vec<ObjectId>,
    remaining_choices: Vec<CopyChosenSelection>,
    params: &CopyChosenParams,
) -> PendingEachPlayerCopyChosen {
    PendingEachPlayerCopyChosen {
        stage,
        player,
        chosen,
        remaining_choices,
        choose_filter: params.choose_filter.clone(),
        min: params.min,
        max: params.max,
        copy_modifications: params.copy_modifications.clone(),
        scale: params.scale.clone(),
        choose_scope: params.choose_scope,
        source_id: params.source_id,
        source_controller: params.source_controller,
        scoped_players: params.scoped_players.clone(),
        trigger_event: params.trigger_event.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, ObjectProperty, QuantityModification, ReplacementDefinition, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CardType, CoreType, Supertype};
    use crate::types::counter::CounterType;
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::resolution::{FrameKind, ResolutionStateWire};
    use crate::types::zones::Zone;

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature())
    }

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state
    }

    fn add_creature(
        state: &mut GameState,
        card_id: CardId,
        player: PlayerId,
        name: &str,
        power: i32,
        legendary: bool,
    ) -> ObjectId {
        let id = create_object(state, card_id, player, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_power = Some(power);
        obj.base_toughness = Some(power);
        obj.power = Some(power);
        obj.toughness = Some(power);
        obj.base_card_types = CardType {
            supertypes: if legendary {
                vec![Supertype::Legendary]
            } else {
                vec![]
            },
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        obj.card_types = obj.base_card_types.clone();
        id
    }

    fn ability(
        min: u32,
        max: u32,
        copy_modifications: Vec<ContinuousModification>,
        scale: Option<CopyScale>,
    ) -> ResolvedAbility {
        ability_scoped(
            min,
            max,
            copy_modifications,
            scale,
            CopyChooseScope::Chooser,
        )
    }

    fn ability_scoped(
        min: u32,
        max: u32,
        copy_modifications: Vec<ContinuousModification>,
        scale: Option<CopyScale>,
        choose_scope: CopyChooseScope,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::EachPlayerCopyChosen {
                choose_filter: creature_filter(),
                min,
                max,
                copy_modifications,
                scale,
                choose_scope,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        )
    }

    fn token_count(state: &GameState) -> usize {
        state
            .battlefield
            .iter()
            .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
            .count()
    }

    #[test]
    fn two_eligible_seeds_selection() {
        let mut state = setup();
        add_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, false);
        add_creature(&mut state, CardId(2), PlayerId(0), "Lion", 3, false);
        let ab = ability(1, 2, vec![], None);
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::EachPlayerCopyChosenSelection {
                player,
                eligible,
                min,
                max,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(eligible.len(), 2, "both P0 creatures eligible");
                assert_eq!((*min, *max), (1, 2));
            }
            other => panic!("expected EachPlayerCopyChosenSelection, got {other:?}"),
        }
    }

    #[test]
    fn zero_eligible_player_is_skipped() {
        let mut state = setup();
        // No creatures at all — both players skip, terminal completion.
        let ab = ability(1, 2, vec![], None);
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::EachPlayerCopyChosenSelection { .. }
            ),
            "no eligible objects → no selection prompt"
        );
    }

    #[test]
    fn single_eligible_auto_resolves_to_nonlegendary_copy() {
        let mut state = setup();
        // P0 has exactly one legendary creature; P1 has none → forced single.
        add_creature(&mut state, CardId(1), PlayerId(0), "Legend", 4, true);
        let ab = ability(
            1,
            2,
            vec![ContinuousModification::RemoveSupertype {
                supertype: Supertype::Legendary,
            }],
            Some(CopyScale {
                counter_type: CounterType::Plus1Plus1,
                scale_property: ObjectProperty::Power,
            }),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();
        // No selection pending — auto-resolved.
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::EachPlayerCopyChosenSelection { .. }
        ));
        assert_eq!(state.last_created_token_ids.len(), 1, "one copy created");
        let token = state
            .objects
            .get(&state.last_created_token_ids[0])
            .expect("token exists");
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "the copy must NOT be legendary"
        );
        // Only one creature chosen → no scaling counters even though scale is set.
        assert_eq!(
            token
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0,
            "single-choice copy gets no +1/+1 counters"
        );
    }

    /// Runtime proof (card-test): the `WaitingFor::EachPlayerCopyChosenSelection`
    /// round-trip drives the copy AND the scale step. P0 chooses two legendary
    /// creatures (a 3/3 copied first, a 2/2 scaler second); the resulting token
    /// is a non-legendary copy of the 3/3 carrying two +1/+1 counters (= the 2/2's
    /// power), controlled by P0. Exercised end-to-end through `engine::apply`.
    #[test]
    fn two_chosen_scales_copy_by_second_power_via_select_targets() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup();
        let c1 = add_creature(&mut state, CardId(1), PlayerId(0), "Big", 3, true);
        let c2 = add_creature(&mut state, CardId(2), PlayerId(0), "Small", 2, true);
        let ab = ability(
            1,
            2,
            vec![ContinuousModification::RemoveSupertype {
                supertype: Supertype::Legendary,
            }],
            Some(CopyScale {
                counter_type: CounterType::Plus1Plus1,
                scale_property: ObjectProperty::Power,
            }),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::EachPlayerCopyChosenSelection { .. }
            ),
            "P0 has two eligible creatures → selection prompt"
        );

        // P0 chooses c1 (copied) then c2 (scaler) — order is load-bearing.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(c1), TargetRef::Object(c2)],
            },
        )
        .expect("SelectTargets applies");

        // Exactly one P0-controlled token exists: a non-legendary copy of the 3/3
        // carrying two +1/+1 counters (the 2/2 scaler's power).
        let tokens: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.is_token && o.controller == PlayerId(0))
            })
            .collect();
        assert_eq!(tokens.len(), 1, "one copy token created for P0");
        let token = state.objects.get(&tokens[0]).unwrap();
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "the copy must not be legendary"
        );
        assert_eq!(
            token
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "copy gets +1/+1 counters equal to the second creature's power (2)"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "two-player walk completes after the only chooser resolves"
        );
    }

    #[test]
    fn collects_all_player_choices_before_creating_tokens() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup();
        let p0_only = add_creature(&mut state, CardId(1), PlayerId(0), "P0 Bear", 2, false);
        let p1_first = add_creature(&mut state, CardId(2), PlayerId(1), "P1 Big", 4, false);
        let p1_second = add_creature(&mut state, CardId(3), PlayerId(1), "P1 Small", 1, false);
        let ab = ability(1, 2, vec![], None);
        let mut events = Vec::new();

        resolve(&mut state, &ab, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EachPlayerCopyChosenSelection {
                player,
                all_choices,
                ..
            } => {
                assert_eq!(*player, PlayerId(1), "P0 forced choice is collected first");
                assert_eq!(
                    all_choices.as_slice(),
                    &[CopyChosenSelection {
                        player: PlayerId(0),
                        chosen: vec![p0_only],
                    }]
                );
            }
            other => panic!("expected P1 EachPlayerCopyChosenSelection, got {other:?}"),
        }
        assert_eq!(
            token_count(&state),
            0,
            "no copy is created until all APNAP choices are complete"
        );

        apply(
            &mut state,
            PlayerId(1),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(p1_first), TargetRef::Object(p1_second)],
            },
        )
        .expect("P1 selection applies");

        assert_eq!(
            token_count(&state),
            2,
            "both players' copies are created after the final choice"
        );
    }

    #[test]
    fn select_targets_revalidates_live_eligibility() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup();
        let c1 = add_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, false);
        let c2 = add_creature(&mut state, CardId(2), PlayerId(0), "Lion", 3, false);
        let ab = ability(1, 2, vec![], None);
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();

        state.objects.get_mut(&c2).unwrap().controller = PlayerId(1);
        let err = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(c1), TargetRef::Object(c2)],
            },
        )
        .expect_err("stale prompt selection must be rejected");
        let err = format!("{err:?}");

        assert!(
            err.contains("selected object no longer eligible"),
            "unexpected error: {err}"
        );
        assert_eq!(token_count(&state), 0);
    }

    #[test]
    fn scale_none_creates_copy_without_counter_step() {
        let mut state = setup();
        add_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, false);
        let ab = ability(
            1,
            1,
            vec![ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Menace,
            }],
            None,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();
        assert_eq!(state.last_created_token_ids.len(), 1);
        let token = state
            .objects
            .get(&state.last_created_token_ids[0])
            .expect("token exists");
        assert_eq!(
            token
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
        assert!(
            token.has_keyword(&crate::types::keywords::Keyword::Menace),
            "menace granted to the copy"
        );
    }

    /// Frame matrix: a real selection creates a CopyToken replacement prompt
    /// beneath the EachPlayerCopyChosen parent. The v2 round-trip must preserve
    /// that exact parent/child order so accepting the replacement resumes and
    /// completes the outer APNAP walk.
    #[test]
    fn copy_replacement_pause_keeps_each_player_parent_and_roundtrips_v2() {
        let mut state = setup();
        let doubler_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        {
            let doubler = state.objects.get_mut(&doubler_id).unwrap();
            let definition = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::DOUBLE);
            doubler.base_replacement_definitions = Arc::new(vec![definition.clone()]);
            doubler.replacement_definitions = vec![definition].into();
        }
        let augmenter_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Token Augmenter".to_string(),
            Zone::Battlefield,
        );
        {
            let augmenter = state.objects.get_mut(&augmenter_id).unwrap();
            let definition = ReplacementDefinition::new(ReplacementEvent::CreateToken)
                .token_owner_scope(ControllerRef::You)
                .quantity_modification(QuantityModification::Plus { value: 1 });
            augmenter.base_replacement_definitions = Arc::new(vec![definition.clone()]);
            augmenter.replacement_definitions = vec![definition].into();
        }
        let first = add_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, false);
        add_creature(&mut state, CardId(2), PlayerId(0), "Lion", 3, false);
        let ability = ability(1, 1, vec![], None);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(first)],
            },
        )
        .expect("selection starts the copy-token replacement pipeline");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                candidate_count: 2,
                ..
            }
        ));
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(crate::types::resolution::ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![FrameKind::EachPlayerCopyChosen, FrameKind::CopyToken],
            "the outer selection walk must stay below its active CopyToken child"
        );
        assert!(
            state.active_each_player_copy_chosen().is_none(),
            "top-only access must not search through the active copy child"
        );

        let serialized = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("nested EachPlayerCopyChosen copy prompt serializes as v2");
        assert!(
            serialized.get("pending_each_player_copy_chosen").is_none(),
            "v2 must not emit the removed v1 each-player-copy-chosen field"
        );
        let mut state = serde_json::from_value::<ResolutionStateWire>(serialized)
            .expect("nested EachPlayerCopyChosen copy prompt roundtrips")
            .into_game_state();
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(crate::types::resolution::ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![FrameKind::EachPlayerCopyChosen, FrameKind::CopyToken],
            "v2 must preserve the outer parent beneath its CopyToken child"
        );

        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("replacement choice resumes both the copy and outer walk");
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolution_stack.is_empty());
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::EachPlayerCopyChosen,
                    source_id: ObjectId(500),
                    ..
                }
            )),
            "accepting the child replacement must complete the retained outer walk"
        );
        assert!(
            (3..=4).contains(&state.last_created_token_ids.len()),
            "the resumed copy must apply the selected double/plus ordering"
        );
    }

    /// Runtime direction proof (Caught in a Parallel Universe class): with
    /// `choose_scope: Neighbor { Left }` in a THREE-player ring, each chooser's
    /// pool is their LEFT neighbor's battlefield — a case a 2-player ring cannot
    /// distinguish (there `neighbor(_,Left) == neighbor(_,Right)`). In seat ring
    /// [P0,P1,P2], `neighbor(_,Left) = next_player`: P0←P1, P1←P2, P2←P0 (wrap).
    /// Each chooser has a single distinct creature → all forced singles → the
    /// walk auto-resolves with no prompt. A Left→Right swap in the resolver map
    /// would make P0 copy P2's creature (power 7) instead of P1's (power 6),
    /// failing these assertions.
    #[test]
    fn neighbor_left_scope_draws_from_left_neighbor_three_player() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.active_player = PlayerId(0);
        // One distinct creature per player, distinct base power for a load-bearing
        // direction signal.
        add_creature(&mut state, CardId(1), PlayerId(0), "Alpha", 5, false);
        add_creature(&mut state, CardId(2), PlayerId(1), "Bravo", 6, false);
        add_creature(&mut state, CardId(3), PlayerId(2), "Charlie", 7, false);

        let ab = ability_scoped(
            1,
            1,
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Menace,
            }],
            None,
            CopyChooseScope::Neighbor {
                direction: crate::types::ability::SeatDirection::Left,
            },
        );
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();

        // Forced singles across the ring → no interactive prompt.
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::EachPlayerCopyChosenSelection { .. }
            ),
            "every chooser's left-neighbor pool is a forced single → no prompt"
        );

        // Each player's token copies their LEFT neighbor's creature.
        let token_for = |state: &GameState, owner: PlayerId| -> ObjectId {
            let ids: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|o| o.is_token && o.controller == owner)
                })
                .collect();
            assert_eq!(ids.len(), 1, "exactly one token for {owner:?}");
            ids[0]
        };
        for (owner, want_name, want_power) in [
            (PlayerId(0), "Bravo", 6),   // P0's left neighbor is P1
            (PlayerId(1), "Charlie", 7), // P1's left neighbor is P2
            (PlayerId(2), "Alpha", 5),   // P2's left neighbor is P0 (wrap)
        ] {
            let tok = state.objects.get(&token_for(&state, owner)).unwrap();
            assert_eq!(tok.name, want_name, "{owner:?} copies left neighbor");
            assert_eq!(
                tok.base_power,
                Some(want_power),
                "{owner:?} copies left neighbor's power"
            );
            assert!(
                tok.has_keyword(&Keyword::Menace),
                "copy modification (menace) applied for {owner:?}"
            );
        }
    }

    /// Runtime proof of the neighbor pool + CR 608.2c live revalidation
    /// (2-player). P0's `Neighbor { Left }` pool is P1's battlefield: with P1
    /// controlling two creatures P0 is prompted from P1's creatures (NOT P0's
    /// own). `is_live_eligible_choice` accepts a neighbor's object and rejects a
    /// non-neighbor (P0-controlled) object; the resolved token copies the chosen
    /// neighbor creature.
    #[test]
    fn neighbor_left_scope_prompts_from_neighbor_pool_and_revalidates() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let p0c = add_creature(&mut state, CardId(1), PlayerId(0), "Mine", 2, false);
        let p1a = add_creature(&mut state, CardId(2), PlayerId(1), "NeighborA", 4, false);
        let p1b = add_creature(&mut state, CardId(3), PlayerId(1), "NeighborB", 5, false);

        let ab = ability_scoped(
            1,
            1,
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Menace,
            }],
            None,
            CopyChooseScope::Neighbor {
                direction: crate::types::ability::SeatDirection::Left,
            },
        );
        let mut events = Vec::new();
        resolve(&mut state, &ab, &mut events).unwrap();

        // P0 is prompted from P1's (left-neighbor's) two creatures — not P0's own.
        match &state.waiting_for {
            WaitingFor::EachPlayerCopyChosenSelection {
                player, eligible, ..
            } => {
                assert_eq!(*player, PlayerId(0));
                let ids: Vec<ObjectId> = eligible
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                assert!(
                    ids.contains(&p1a) && ids.contains(&p1b),
                    "P1's two creatures"
                );
                assert!(!ids.contains(&p0c), "P0's own creature is NOT eligible");
            }
            other => panic!("expected P0 EachPlayerCopyChosenSelection, got {other:?}"),
        }

        // CR 608.2c: neighbor revalidation — a non-neighbor (P0-controlled) object
        // is rejected; the neighbor's object is accepted.
        let scope = CopyChooseScope::Neighbor {
            direction: crate::types::ability::SeatDirection::Left,
        };
        let filter = creature_filter();
        assert!(
            !is_live_eligible_choice(
                &state,
                PlayerId(0),
                p0c,
                &filter,
                scope,
                ObjectId(500),
                PlayerId(0)
            ),
            "P0's own creature is not in P0's left-neighbor pool"
        );
        assert!(
            is_live_eligible_choice(
                &state,
                PlayerId(0),
                p1a,
                &filter,
                scope,
                ObjectId(500),
                PlayerId(0)
            ),
            "the left neighbor's creature is eligible"
        );

        // P0 chooses one of P1's creatures → P0's token copies it.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(p1a)],
            },
        )
        .expect("selection applies");

        let p0_tokens: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.is_token && o.controller == PlayerId(0))
            })
            .collect();
        assert_eq!(p0_tokens.len(), 1, "one P0 token");
        let tok = state.objects.get(&p0_tokens[0]).unwrap();
        assert_eq!(
            tok.name, "NeighborA",
            "P0's token copies the chosen neighbor creature"
        );
        assert_eq!(tok.base_power, Some(4));
        assert!(tok.has_keyword(&Keyword::Menace));
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::EachPlayerCopyChosenSelection { .. }
            ),
            "walk completes after the only prompted chooser resolves"
        );
    }
}

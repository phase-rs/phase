//! CR 701.61a: Forage — "Exile three cards from your graveyard or sacrifice a
//! Food." A modal keyword action, performed when an effect instructs a player
//! to "forage."
//!
//! Implemented by composition over existing effects rather than a bespoke
//! `WaitingFor`:
//!   * the exile mode reuses `Effect::ChangeZone`'s resolution-time selection
//!     (`multi_target` fixed at 3 + `TargetChoiceTiming::Resolution` + empty
//!     targets), which routes through the shared `EffectZoneChoice` picker;
//!   * the Food mode reuses `Effect::Sacrifice` (the same machinery Devour and
//!     every "sacrifice a Food" cost use);
//!   * when both modes are performable the controller chooses via
//!     `Effect::ChooseOneOf`.
//!
//! CR 701.61a is atomic per mode — you exile *three* cards or sacrifice *a*
//! Food — so a mode is offered only when it can be performed in full. If
//! neither mode is performable, foraging does nothing.

use crate::game::ability_utils::{append_to_sub_chain, build_resolved_from_def};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, EffectError, EffectKind,
    EffectResolutionResult, FilterProp, MultiTargetSpec, PlayerFilter, QuantityExpr,
    ResolvedAbility, TargetChoiceTiming, TargetFilter, TargetRef, ThisWayCause, TypedFilter,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 701.61a: "exile three cards from your graveyard".
const FORAGE_EXILE_COUNT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForageMode {
    ExileThree,
    SacrificeFood,
}

fn graveyard_size(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .get(player.0 as usize)
        .map(|p| p.graveyard.len())
        .unwrap_or(0)
}

fn controls_food(state: &GameState, player: PlayerId, source_id: ObjectId) -> bool {
    // Reuse the layer-aware, phased-out-aware control-count building block
    // (CR 702.26b) rather than a raw battlefield subtype scan, so the
    // eligibility gate agrees with the `Sacrifice` resolver it gates.
    let filter = TargetFilter::Typed(TypedFilter::permanent().subtype("Food".to_string()));
    super::player_control_count_compares(state, player, &filter, Comparator::GE, 1, source_id)
}

fn available_modes(state: &GameState, player: PlayerId, source_id: ObjectId) -> Vec<ForageMode> {
    let mut modes = Vec::with_capacity(2);
    if graveyard_size(state, player) >= FORAGE_EXILE_COUNT {
        modes.push(ForageMode::ExileThree);
    }
    if controls_food(state, player, source_id) {
        modes.push(ForageMode::SacrificeFood);
    }
    modes
}

pub(crate) fn can_forage(state: &GameState, ability: &ResolvedAbility) -> bool {
    !available_modes(state, ability.controller, ability.source_id).is_empty()
}

fn completion(cause: ThisWayCause, count: usize) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CompletePlayerAction {
            parent_kind: EffectKind::Forage,
            action: PlayerActionKind::Forage,
            required_result: EffectResolutionResult { cause, count },
        },
    )
}

/// CR 701.61a (exile mode): exile three chosen cards from the forager's
/// graveyard. `Owned { You }` scopes the scan to the forager's own graveyard;
/// `MultiTargetSpec::fixed(3, 3)` forces exactly three (eligibility is checked
/// before this branch is offered).
fn exile_three_branch() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Owned {
                controller: ControllerRef::You,
            }])),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    )
    .multi_target(MultiTargetSpec::fixed(
        FORAGE_EXILE_COUNT,
        FORAGE_EXILE_COUNT,
    ))
    .target_choice_timing(TargetChoiceTiming::Resolution)
    .description("Exile three cards from your graveyard.".to_string())
    .sub_ability(completion(ThisWayCause::Exiled, FORAGE_EXILE_COUNT))
}

/// CR 701.61a (Food mode): sacrifice a Food the forager controls.
fn sacrifice_food_branch() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .subtype("Food".to_string()),
            ),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 1,
        },
    )
    .description("Sacrifice a Food.".to_string())
    .sub_ability(completion(ThisWayCause::Sacrificed, 1))
}

/// CR 701.61a: resolve a "forage" instruction. Offers only the performable
/// mode(s); performs the single mode directly, prompts a `ChooseOneOf` when
/// both are available, and is a no-op when neither is.
pub(crate) fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let controller = ability.controller;
    let modes = available_modes(state, controller, ability.source_id);
    let mut branches: Vec<AbilityDefinition> = modes
        .iter()
        .map(|mode| match mode {
            ForageMode::ExileThree => exile_three_branch(),
            ForageMode::SacrificeFood => sacrifice_food_branch(),
        })
        .collect();
    let mut tail = ability.sub_ability.as_deref().cloned();
    if let Some(tail) = tail.as_mut() {
        tail.clear_prior_effect_result_recursive();
    }

    match branches.len() {
        // CR 701.61a: neither mode performable — foraging does nothing.
        0 => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Forage,
                source_id: ability.source_id,
                subject: None,
            });
            if let Some(mut tail) = tail {
                tail.set_optional_effect_performed_recursive(false);
                super::resolve_ability_chain(state, &tail, events, 1)?;
            }
        }
        // Exactly one performable mode — perform it directly (no modal prompt).
        1 => {
            let branch = branches.pop().expect("len checked == 1");
            let mut resolved = build_resolved_from_def(&branch, ability.source_id, controller);
            resolved.context = ability.context.clone();
            resolved.clear_prior_effect_result_recursive();
            resolved.set_scoped_player_recursive(controller);
            if let Some(tail) = tail {
                append_to_sub_chain(&mut resolved, tail);
            }
            // Depth 1, not 0: `forage::resolve` already runs inside a resolution,
            // so a depth-0 re-entry would re-run the depth-0 prelude mid-resolution
            // (clearing chain-scoped state, re-bumping counters). Matches the
            // depth-1 branch resolution the two-mode `choose_one_of` path uses.
            super::resolve_ability_chain(state, &resolved, events, 1)?;
        }
        // CR 701.61a: both modes available — the forager chooses which.
        _ => {
            let mut choose = ResolvedAbility::new(
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches,
                },
                vec![TargetRef::Player(controller)],
                ability.source_id,
                controller,
            );
            choose.context = ability.context.clone();
            choose.clear_prior_effect_result_recursive();
            choose.sub_ability = tail.map(Box::new);
            super::choose_one_of::resolve(state, &choose, events)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityCondition, EffectOutcomeSignal, SubAbilityLink};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{PendingContinuation, WaitingFor};
    use crate::types::identifiers::{CardId, ObjectId};

    fn forage_ability(controller: PlayerId, source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Forage, vec![], source, controller)
    }

    fn add_graveyard_card(state: &mut GameState, owner: PlayerId, n: u64) -> ObjectId {
        create_object(state, CardId(n), owner, format!("GY{n}"), Zone::Graveyard)
    }

    fn add_food(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(900),
            owner,
            "Food Token".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.controller = owner;
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Food".to_string()];
        id
    }

    fn pending_choice(state: &GameState) -> bool {
        matches!(
            state.waiting_for,
            WaitingFor::EffectZoneChoice { .. } | WaitingFor::ChooseOneOfBranch { .. }
        )
    }

    /// CR 701.61a: with neither three graveyard cards nor a Food, foraging does nothing.
    #[test]
    fn forage_with_neither_mode_is_noop() {
        let mut state = GameState::new_two_player(1);
        let mut events = Vec::new();
        resolve(
            &mut state,
            &forage_ability(PlayerId(0), ObjectId(1)),
            &mut events,
        )
        .unwrap();
        assert!(!pending_choice(&state), "no choice should be set up");
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Forage,
                ..
            }
        )));
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Forage,
                    ..
                }
            )),
            "an impossible forage must not emit a player-action event"
        );
    }

    /// CR 608.2c + CR 609.3: a zero-mode Forage is a failed action for an
    /// `IfYouDo` rider, while a separate unconditional printed instruction
    /// remains independent and still resolves.
    #[test]
    fn zero_mode_gates_if_you_do_but_runs_unconditional_sibling() {
        let source = ObjectId(49);
        let tail = |condition| {
            let mut tail = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                Vec::new(),
                source,
                PlayerId(0),
            );
            tail.condition = condition;
            tail.sub_link = SubAbilityLink::SequentialSibling;
            tail
        };

        let mut gated_state = GameState::new_two_player(1);
        let gated = forage_ability(PlayerId(0), source).sub_ability(tail(Some(
            AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed,
            },
        )));
        resolve(&mut gated_state, &gated, &mut Vec::new()).unwrap();
        assert_eq!(gated_state.players[0].life, 20);

        let mut independent_state = GameState::new_two_player(1);
        let independent = forage_ability(PlayerId(0), source).sub_ability(tail(None));
        resolve(&mut independent_state, &independent, &mut Vec::new()).unwrap();
        assert_eq!(independent_state.players[0].life, 21);
    }

    /// CR 701.61a (exile mode): three graveyard cards and no Food prompts an
    /// exile-three-from-your-graveyard selection (Graveyard -> Exile, count 3).
    #[test]
    fn forage_exile_only_prompts_exile_three_from_graveyard() {
        let mut state = GameState::new_two_player(1);
        for n in 1..=3 {
            add_graveyard_card(&mut state, PlayerId(0), n);
        }
        let mut events = Vec::new();
        resolve(
            &mut state,
            &forage_ability(PlayerId(0), ObjectId(50)),
            &mut events,
        )
        .unwrap();
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                count,
                zone,
                destination,
                ..
            } => {
                assert_eq!(*count, 3);
                assert_eq!(*zone, Zone::Graveyard);
                assert_eq!(*destination, Some(Zone::Exile));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    /// CR 701.61a: the exile mode is atomic (three cards) — fewer than three
    /// graveyard cards (and no Food) makes foraging a no-op, never a partial exile.
    #[test]
    fn forage_fewer_than_three_in_graveyard_does_nothing() {
        let mut state = GameState::new_two_player(1);
        add_graveyard_card(&mut state, PlayerId(0), 1);
        add_graveyard_card(&mut state, PlayerId(0), 2);
        let mut events = Vec::new();
        resolve(
            &mut state,
            &forage_ability(PlayerId(0), ObjectId(51)),
            &mut events,
        )
        .unwrap();
        assert!(!pending_choice(&state));
    }

    /// CR 701.61a (Food mode): a Food with fewer than three graveyard cards
    /// sacrifices the Food (the only performable mode), no modal prompt.
    #[test]
    fn forage_food_only_sacrifices_the_food() {
        let mut state = GameState::new_two_player(1);
        let food = add_food(&mut state, PlayerId(0));
        let mut events = Vec::new();
        resolve(
            &mut state,
            &forage_ability(PlayerId(0), ObjectId(52)),
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&food).map(|o| o.zone),
            Some(Zone::Graveyard),
            "the only Food should have been sacrificed"
        );
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id: PlayerId(0),
                action: PlayerActionKind::Forage,
                ..
            }
        )));
    }

    /// CR 701.61a: both modes available — the forager chooses which via a modal prompt.
    #[test]
    fn forage_both_modes_prompts_choose_one_of() {
        let mut state = GameState::new_two_player(1);
        for n in 1..=3 {
            add_graveyard_card(&mut state, PlayerId(0), n);
        }
        add_food(&mut state, PlayerId(0));
        let mut events = Vec::new();
        resolve(
            &mut state,
            &forage_ability(PlayerId(0), ObjectId(53)),
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "expected a modal choice, got {:?}",
            state.waiting_for
        );
    }

    /// CR 608.2c + CR 701.55d: when both Forage modes are legal, the runtime
    /// printed tail is carried by the modal choice and attached only after the
    /// selected branch's completion node. A successful Food branch therefore
    /// runs an `IfYouDo` tail exactly once.
    #[test]
    fn two_mode_choice_runs_success_gated_runtime_tail_once() {
        let mut state = GameState::new_two_player(1);
        for n in 1..=3 {
            add_graveyard_card(&mut state, PlayerId(0), n);
        }
        let food = add_food(&mut state, PlayerId(0));
        let source = ObjectId(54);
        let mut tail = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        tail.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        tail.sub_link = SubAbilityLink::SequentialSibling;
        let ability = forage_ability(PlayerId(0), source).sub_ability(tail);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        let serialized = serde_json::to_string(&state).expect("modal Forage state serializes");
        state = serde_json::from_str(&serialized)
            .expect("modal Forage state with runtime tail deserializes");
        let food_branch = match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch { branches, .. } => branches
                .iter()
                .position(|branch| matches!(branch.effect.as_ref(), Effect::Sacrifice { .. }))
                .expect("Food branch must be offered"),
            other => panic!("expected modal Forage choice, got {other:?}"),
        };
        assert_eq!(state.players[0].life, 20);

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: food_branch },
        )
        .expect("choose Food forage branch");

        assert_eq!(state.objects[&food].zone, Zone::Graveyard);
        assert_eq!(state.players[0].life, 21);
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: PlayerActionKind::Forage,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    /// CR 608.2c: a synchronous sacrifice returns its result to its own direct
    /// child. It must not stamp an already-active, same-source completion frame
    /// merely because that unrelated frame asks for the same cause and count.
    #[test]
    fn synchronous_food_result_does_not_stamp_unrelated_same_source_continuation() {
        let mut state = GameState::new_two_player(1);
        add_food(&mut state, PlayerId(0));
        let source = ObjectId(55);
        let unrelated = build_resolved_from_def(
            &completion(ThisWayCause::Sacrificed, 1),
            source,
            PlayerId(0),
        );
        let pending = PendingContinuation::new(Box::new(unrelated), &state);
        state.park_ability_continuation(pending);
        let sacrifice = build_resolved_from_def(&sacrifice_food_branch(), source, PlayerId(0));
        let mut events = Vec::new();

        let result =
            crate::game::effects::sacrifice::resolve(&mut state, &sacrifice, &mut events).unwrap();

        assert_eq!(
            result,
            Some(EffectResolutionResult {
                cause: ThisWayCause::Sacrificed,
                count: 1,
            })
        );
        assert_eq!(
            state
                .active_ability_continuation()
                .expect("hostile continuation remains parked")
                .chain
                .context
                .prior_effect_result,
            None,
            "a synchronous operation cannot stamp an unrelated active frame"
        );
    }
}

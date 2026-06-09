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

use crate::game::ability_utils::build_resolved_from_def;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, EffectError, EffectKind,
    FilterProp, MultiTargetSpec, PlayerFilter, QuantityExpr, ResolvedAbility, TargetChoiceTiming,
    TargetFilter, TargetRef, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 701.61a: "exile three cards from your graveyard".
const FORAGE_EXILE_COUNT: usize = 3;

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
            face_down_profile: None,
        },
    )
    .multi_target(MultiTargetSpec::fixed(
        FORAGE_EXILE_COUNT,
        FORAGE_EXILE_COUNT,
    ))
    .target_choice_timing(TargetChoiceTiming::Resolution)
    .description("Exile three cards from your graveyard.".to_string())
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

    let mut branches: Vec<AbilityDefinition> = Vec::new();
    if graveyard_size(state, controller) >= FORAGE_EXILE_COUNT {
        branches.push(exile_three_branch());
    }
    if controls_food(state, controller, ability.source_id) {
        branches.push(sacrifice_food_branch());
    }

    match branches.len() {
        // CR 701.61a: neither mode performable — foraging does nothing.
        0 => {}
        // Exactly one performable mode — perform it directly (no modal prompt).
        1 => {
            let branch = branches.pop().expect("len checked == 1");
            let mut resolved = build_resolved_from_def(&branch, ability.source_id, controller);
            resolved.context = ability.context.clone();
            resolved.set_scoped_player_recursive(controller);
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
            super::choose_one_of::resolve(state, &choose, events)?;
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Forage,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::WaitingFor;
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
}

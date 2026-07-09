//! CR 700.3 + CR 608: Pile-separation primitive — partition objects into two
//! piles, another player chooses one, sub-effect applies.
//!
//! Per CR 700.3b, a pile is not a `GameObject`; it is a transient
//! `im::Vector<ObjectId>` ledger that lives on the [`WaitingFor`] until the
//! chooser picks a side. Per CR 700.3c, partitioned objects do not leave
//! their zone during the partition/choice steps — only the final sub-effect
//! acts on them. Per CR 700.3a the partition is exhaustive and disjoint
//! (pile B is derived as `eligible \ pile_a`) and per CR 700.3d either pile
//! may be empty.
//!
//! This module follows the Vote interactive-queue pattern: build an
//! APNAP-ordered subject queue, park on a dedicated `WaitingFor` for the
//! first subject, and process advance/transition in
//! `engine_resolution_choices.rs`. The chosen-pile sub-effect is fanned out
//! from the choice handler.

use crate::game::players::apnap_order_from;
use crate::types::ability::{
    Effect, EffectError, EffectKind, PileSource, PlayerScope, ResolvedAbility, VoterScope,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PileResult, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 700.3 + CR 101.4: Initiate a pile-separation effect. Dispatches on
/// `pile_source` to either the battlefield path (Make an Example) or the
/// revealed-from-library-top path (Fact or Fiction).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::SeparateIntoPiles {
        partition_subject,
        object_filter,
        chooser,
        chosen_pile_effect,
        pile_source,
        unchosen_pile_effect,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "separate_piles::resolve called with non-SeparateIntoPiles effect".into(),
        ));
    };

    let controller = ability.controller;
    let chooser_id = resolve_chooser(state, ability, chooser.clone()).unwrap_or(controller);

    match pile_source {
        PileSource::Battlefield => resolve_battlefield(
            state,
            ability,
            events,
            partition_subject,
            object_filter,
            chooser_id,
            chosen_pile_effect,
            unchosen_pile_effect,
        ),
        PileSource::RevealedFromLibraryTop { count } => resolve_revealed_from_library_top(
            state,
            ability,
            events,
            *count,
            chooser_id,
            chosen_pile_effect,
            unchosen_pile_effect,
        ),
    }
}

/// CR 700.3 + CR 700.3c: Battlefield pile source — the Make an Example path.
#[allow(clippy::too_many_arguments)]
fn resolve_battlefield(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    partition_subject: &VoterScope,
    object_filter: &crate::types::ability::TargetFilter,
    chooser_id: PlayerId,
    chosen_pile_effect: &crate::types::ability::AbilityDefinition,
    unchosen_pile_effect: &Option<Box<crate::types::ability::AbilityDefinition>>,
) -> Result<(), EffectError> {
    let controller = ability.controller;

    // CR 101.4: APNAP order starting at the active player; CR 800.4f drops
    // eliminated players.
    let subjects: Vec<PlayerId> = apnap_order_from(state, None, controller)
        .into_iter()
        .filter(|pid| match partition_subject {
            // CR 800.4g: `EachOpponent` excludes the controller.
            VoterScope::EachOpponent | VoterScope::AnOpponent => *pid != controller,
            VoterScope::AllPlayers => true,
            VoterScope::ControllerLabels => false,
        })
        .collect();

    // CR 700.3 + CR 700.3c: Compute each subject's eligible objects.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let mut subject_pools: Vec<(PlayerId, crate::im::Vector<ObjectId>)> = subjects
        .into_iter()
        .map(|pid| {
            let pool: crate::im::Vector<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == pid
                            && !obj.is_emblem
                            && crate::game::filter::matches_target_filter(
                                state,
                                *id,
                                object_filter,
                                &ctx,
                            )
                    })
                })
                .collect();
            (pid, pool)
        })
        .collect();

    // CR 700.3d: Subjects with zero eligible objects are recorded as empty
    // partitions and skipped.
    let mut completed: crate::im::Vector<PileResult> = crate::im::Vector::new();
    while let Some((pid, pool)) = subject_pools.first() {
        if pool.is_empty() {
            completed.push_back(PileResult {
                subject: *pid,
                pile_a: crate::im::Vector::new(),
                pile_b: crate::im::Vector::new(),
            });
            subject_pools.remove(0);
        } else {
            break;
        }
    }

    if subject_pools.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SeparateIntoPiles,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let (first_subject, first_pool) = subject_pools.remove(0);
    let remaining_subjects: crate::im::Vector<(PlayerId, crate::im::Vector<ObjectId>)> =
        subject_pools.into_iter().collect();

    state.waiting_for = WaitingFor::SeparatePilesPartition {
        player: first_subject,
        eligible: first_pool,
        remaining_subjects,
        completed,
        chooser: chooser_id,
        chosen_pile_effect: Box::new(chosen_pile_effect.clone()),
        unchosen_pile_effect: unchosen_pile_effect.clone(),
        source_id: ability.source_id,
    };

    Ok(())
}

/// CR 700.3 + CR 608.2d: RevealedFromLibraryTop pile source — the Fact or Fiction
/// path. Reveal top N cards, an opponent separates them into two piles, controller
/// chooses one pile.
#[allow(clippy::too_many_arguments)]
fn resolve_revealed_from_library_top(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    count: u32,
    chooser_id: PlayerId,
    chosen_pile_effect: &crate::types::ability::AbilityDefinition,
    unchosen_pile_effect: &Option<Box<crate::types::ability::AbilityDefinition>>,
) -> Result<(), EffectError> {
    let controller = ability.controller;

    // CR 609.3: If an effect attempts to do something impossible, it does only as much as possible.
    let player = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .ok_or(EffectError::PlayerNotFound)?;
    let reveal_count = (count as usize).min(player.library.len());

    if reveal_count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SeparateIntoPiles,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let revealed_ids: Vec<ObjectId> = player.library.iter().take(reveal_count).copied().collect();

    // CR 701.20a: Mark cards as revealed and emit CardsRevealed event.
    for &card_id in &revealed_ids {
        state.revealed_cards.insert(card_id);
    }
    state.last_revealed_ids = revealed_ids.clone();
    let card_names: Vec<String> = revealed_ids
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: controller,
        card_ids: revealed_ids.clone(),
        card_names,
    });

    // CR 608.2d + CR 700.3: "An opponent" — the controller chooses which opponent
    // performs the partition. With a single opponent the choice is trivial.
    let candidates: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| p.id != controller && !p.is_eliminated)
        .map(|p| p.id)
        .collect();

    let eligible: crate::im::Vector<ObjectId> = revealed_ids.into_iter().collect();

    if candidates.len() >= 2 {
        // Multiplayer: surface a choice prompt for the controller.
        state.waiting_for = WaitingFor::SeparatePilesChooseOpponent {
            player: controller,
            candidates,
            eligible,
            chooser: chooser_id,
            chosen_pile_effect: Box::new(chosen_pile_effect.clone()),
            unchosen_pile_effect: unchosen_pile_effect.clone(),
            source_id: ability.source_id,
        };
    } else {
        // Two-player game: single opponent, no decision needed.
        let partitioner = candidates.into_iter().next().unwrap_or(controller);
        state.waiting_for = WaitingFor::SeparatePilesPartition {
            player: partitioner,
            eligible,
            remaining_subjects: crate::im::Vector::new(),
            completed: crate::im::Vector::new(),
            chooser: chooser_id,
            chosen_pile_effect: Box::new(chosen_pile_effect.clone()),
            unchosen_pile_effect: unchosen_pile_effect.clone(),
            source_id: ability.source_id,
        };
    }

    Ok(())
}

/// CR 700.3 + CR 109.4: Apply the chosen-pile sub-effect across every
/// completed subject.
pub fn apply_pile_effect(
    state: &mut GameState,
    source_id: ObjectId,
    chosen_pile_effect: &crate::types::ability::AbilityDefinition,
    results: &[(PileResult, crate::types::game_state::PileSide)],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for (result, side) in results {
        let chosen: &crate::im::Vector<ObjectId> = match side {
            crate::types::game_state::PileSide::A => &result.pile_a,
            crate::types::game_state::PileSide::B => &result.pile_b,
        };
        if chosen.is_empty() {
            continue;
        }
        for &object_id in chosen.iter() {
            let mut chain = sub_effect_as_resolved(chosen_pile_effect, source_id, result.subject);
            chain.targets = vec![crate::types::ability::TargetRef::Object(object_id)];
            super::resolve_ability_chain(state, &chain, events, 1)?;
        }
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SeparateIntoPiles,
        source_id,
    });
    Ok(())
}

/// CR 700.3 + CR 608.2c: Apply the unchosen-pile sub-effect across the
/// unchosen pile objects.
pub fn apply_unchosen_pile_effect(
    state: &mut GameState,
    source_id: ObjectId,
    unchosen_pile_effect: &crate::types::ability::AbilityDefinition,
    results: &[(PileResult, crate::types::game_state::PileSide)],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for (result, chosen_side) in results {
        let unchosen: &crate::im::Vector<ObjectId> = match chosen_side {
            crate::types::game_state::PileSide::A => &result.pile_b,
            crate::types::game_state::PileSide::B => &result.pile_a,
        };
        if unchosen.is_empty() {
            continue;
        }
        for &object_id in unchosen.iter() {
            let mut chain = sub_effect_as_resolved(unchosen_pile_effect, source_id, result.subject);
            chain.targets = vec![crate::types::ability::TargetRef::Object(object_id)];
            super::resolve_ability_chain(state, &chain, events, 1)?;
        }
    }
    Ok(())
}

/// Convert a parsed `AbilityDefinition` into a `ResolvedAbility`.
fn sub_effect_as_resolved(
    def: &crate::types::ability::AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    let mut resolved =
        ResolvedAbility::new((*def.effect).clone(), Vec::new(), source_id, controller);
    resolved.kind = def.kind;
    resolved.sub_ability = def
        .sub_ability
        .as_ref()
        .map(|sub| Box::new(sub_effect_as_resolved(sub, source_id, controller)));
    resolved.duration = def.duration.clone();
    resolved.condition = def.condition.clone();
    resolved.optional_targeting = def.optional_targeting;
    resolved.optional = def.optional;
    resolved.target_choice_timing = def.target_choice_timing;
    resolved.description = def.description.clone();
    resolved.min_x_value = def.min_x_value;
    resolved.cant_be_copied = def.cant_be_copied;
    resolved.forward_result = def.forward_result;
    resolved.player_scope = def.player_scope.clone();
    resolved.starting_with = def.starting_with.clone();
    resolved.target_selection_mode = def.target_selection_mode;
    resolved.sub_link = def.sub_link;
    resolved
}

/// CR 109.4 + CR 608.2c: Resolve a `PlayerScope` to the concrete chooser.
fn resolve_chooser(
    _state: &GameState,
    ability: &ResolvedAbility,
    chooser: PlayerScope,
) -> Option<PlayerId> {
    match chooser {
        PlayerScope::Controller => Some(ability.controller),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    fn sacrifice_sub() -> Box<AbilityDefinition> {
        Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        ))
    }

    fn make_an_example_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SeparateIntoPiles {
                partition_subject: VoterScope::EachOpponent,
                object_filter: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                chooser: PlayerScope::Controller,
                chosen_pile_effect: sacrifice_sub(),
                pile_source: PileSource::Battlefield,
                unchosen_pile_effect: None,
            },
            Vec::new(),
            source_id,
            controller,
        )
    }

    fn place_creature(state: &mut GameState, owner: PlayerId, card_id: u64) -> ObjectId {
        let id = crate::game::zones::create_object(
            state,
            CardId(card_id),
            owner,
            format!("C{card_id}"),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        id
    }

    /// CR 700.3 + CR 800.4g: Initiating Make an Example with an opponent who
    /// controls creatures parks on `SeparatePilesPartition` for that opponent.
    #[test]
    fn make_an_example_parks_on_opponent_partition() {
        let mut state = GameState::new_two_player(42);
        let caster = state.players[0].id;
        let opp = state.players[1].id;
        let c1 = place_creature(&mut state, opp, 1);
        let c2 = place_creature(&mut state, opp, 2);
        let c3 = place_creature(&mut state, opp, 3);

        let ability = make_an_example_ability(ObjectId(100), caster);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("resolves");
        match &state.waiting_for {
            WaitingFor::SeparatePilesPartition {
                player,
                eligible,
                chooser,
                ..
            } => {
                assert_eq!(*player, opp);
                assert_eq!(*chooser, caster);
                assert!(eligible.contains(&c1));
                assert!(eligible.contains(&c2));
                assert!(eligible.contains(&c3));
                assert_eq!(eligible.len(), 3);
            }
            other => panic!("expected SeparatePilesPartition, got {other:?}"),
        }
    }

    /// CR 700.3d: An opponent with no creatures is recorded as an empty
    /// `PileResult` and skipped.
    #[test]
    fn empty_opponent_pools_skip_to_completion() {
        let mut state = GameState::new_two_player(42);
        let caster = state.players[0].id;
        let ability = make_an_example_ability(ObjectId(100), caster);
        let mut events = Vec::new();
        let initial = state.waiting_for.clone();
        resolve(&mut state, &ability, &mut events).expect("resolves");
        assert!(matches!(state.waiting_for, ref w if *w == initial));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::SeparateIntoPiles,
                ..
            }
        )));
    }

    /// CR 700.3 / CR 701.21: End-to-end runtime test driving Make an Example.
    #[test]
    fn discriminator_make_an_example_sacrifices_chosen_pile() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;
        use crate::types::game_state::PileSide;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = state.players[0].id;
        state.priority_player = state.players[0].id;
        state.waiting_for = WaitingFor::Priority {
            player: state.players[0].id,
        };

        let caster = state.players[0].id;
        let opp = state.players[1].id;
        let c1 = place_creature(&mut state, opp, 10);
        let c2 = place_creature(&mut state, opp, 11);
        let c3 = place_creature(&mut state, opp, 12);

        let ability = make_an_example_ability(ObjectId(500), caster);
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("resolves the chain");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SeparatePilesPartition { player, .. } if player == opp
        ));

        apply(
            &mut state,
            opp,
            GameAction::SubmitPilePartition {
                pile_a: vec![c1, c2],
            },
        )
        .expect("partition accepted");

        assert!(matches!(
            state.waiting_for,
            WaitingFor::SeparatePilesChoice { player, .. } if player == caster
        ));
        apply(
            &mut state,
            caster,
            GameAction::ChoosePile { pile: PileSide::A },
        )
        .expect("pile choice accepted");

        assert!(!state.battlefield.contains(&c1), "c1 must be sacrificed");
        assert!(!state.battlefield.contains(&c2), "c2 must be sacrificed");
        assert!(
            state.battlefield.contains(&c3),
            "c3 (in unchosen pile) must remain on battlefield"
        );
        assert!(state.players[1].graveyard.contains(&c1));
        assert!(state.players[1].graveyard.contains(&c2));
    }

    /// CR 700.3d: When the chooser picks the empty pile, zero creatures are
    /// sacrificed and no panic occurs.
    #[test]
    fn empty_pile_choice_sacrifices_nothing() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;
        use crate::types::game_state::PileSide;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = state.players[0].id;
        state.priority_player = state.players[0].id;
        state.waiting_for = WaitingFor::Priority {
            player: state.players[0].id,
        };
        let caster = state.players[0].id;
        let opp = state.players[1].id;
        let c1 = place_creature(&mut state, opp, 20);
        let c2 = place_creature(&mut state, opp, 21);

        let ability = make_an_example_ability(ObjectId(600), caster);
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("resolves the chain");
        apply(
            &mut state,
            opp,
            GameAction::SubmitPilePartition {
                pile_a: vec![c1, c2],
            },
        )
        .expect("partition accepted");
        apply(
            &mut state,
            caster,
            GameAction::ChoosePile { pile: PileSide::B },
        )
        .expect("empty-pile choice accepted");
        assert!(state.battlefield.contains(&c1));
        assert!(state.battlefield.contains(&c2));
        assert!(state.players[1].graveyard.is_empty());
    }
}

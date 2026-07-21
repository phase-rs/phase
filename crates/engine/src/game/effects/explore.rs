use std::collections::HashSet;

use crate::game::filter;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};

use super::resolve_ability_chain;

/// Add a +1/+1 counter to the exploring creature via the replacement pipeline.
fn add_explore_counter(state: &mut GameState, explorer_id: ObjectId, events: &mut Vec<GameEvent>) {
    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Object {
            actor: state
                .objects
                .get(&explorer_id)
                .map(|obj| obj.controller)
                .unwrap_or(PlayerId(0)),
            object_id: explorer_id,
            counter_type: CounterType::Plus1Plus1,
        },
        count: 1,
        applied: HashSet::new(),
    };

    if let ReplacementResult::Execute(ProposedEvent::AddCounter {
        placement:
            CounterPlacement::Object {
                actor,
                object_id,
                counter_type,
            },
        count,
        ..
    }) = replacement::replace_event(state, proposed, events)
    {
        super::counters::apply_counter_addition(
            state,
            actor,
            object_id,
            counter_type,
            count,
            events,
        );
    }
}

fn next_explore_chooser(
    state: &GameState,
    remaining: &[ObjectId],
) -> Option<(PlayerId, Vec<ObjectId>)> {
    let apnap = crate::game::players::apnap_order(state);
    for player in apnap {
        let choosable: Vec<ObjectId> = remaining
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|object| object.controller == player)
            })
            .collect();
        if !choosable.is_empty() {
            return Some((player, choosable));
        }
    }
    None
}

fn collect_explorers(
    state: &GameState,
    ability: &ResolvedAbility,
    filter_spec: &TargetFilter,
) -> Vec<ObjectId> {
    match filter_spec {
        TargetFilter::ParentTarget => ability
            .targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
            .filter(|obj_id| state.objects.contains_key(obj_id))
            .collect(),
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|obj_id| state.objects.contains_key(obj_id))
            .collect(),
        _ => {
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            let ctx = filter::FilterContext::from_ability(ability);
            state
                .battlefield
                .iter()
                .copied()
                .filter(|obj_id| filter::matches_target_filter(state, *obj_id, filter_spec, &ctx))
                .collect()
        }
    }
}

fn continuation_for_remaining(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: Vec<ObjectId>,
) -> Option<ResolvedAbility> {
    if remaining.is_empty() {
        return None;
    }

    let tracked_set_id = crate::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(tracked_set_id, remaining);

    Some(
        ResolvedAbility::new(
            Effect::ExploreAll {
                filter: TargetFilter::TrackedSet { id: tracked_set_id },
            },
            vec![],
            ability.source_id,
            ability.controller,
        )
        .kind(ability.kind)
        .context(ability.context.clone()),
    )
}

fn resolve_single_explorer(
    state: &mut GameState,
    ability: &ResolvedAbility,
    explorer_id: ObjectId,
    remaining: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut single = ResolvedAbility::new(
        Effect::Explore,
        vec![TargetRef::Object(explorer_id)],
        ability.source_id,
        ability.controller,
    )
    .kind(ability.kind)
    .context(ability.context.clone());

    if let Some(next) = continuation_for_remaining(state, ability, remaining) {
        single = single.sub_ability(next);
    } else if let Some(sub) = ability.sub_ability.as_deref() {
        single = single.sub_ability(sub.clone());
    }

    resolve_ability_chain(state, &single, events, 1)
}

fn advance_explore_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some((player, choosable)) = next_explore_chooser(state, &remaining) else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    if choosable.len() == 1 {
        let chosen = choosable[0];
        let remaining: Vec<ObjectId> = remaining
            .into_iter()
            .filter(|obj_id| *obj_id != chosen)
            .collect();
        return resolve_single_explorer(state, ability, chosen, remaining, events);
    }

    state.waiting_for = WaitingFor::ExploreChoice {
        player,
        source_id: ability.source_id,
        choosable,
        remaining,
        pending_effect: Box::new(ability.clone()),
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 701.44a: Explore — reveal the top card of the exploring creature's controller's library.
/// - If the card is a land: put it into that player's hand (no counter).
/// - If the card is not a land: put a +1/+1 counter on the creature, then the player
///   chooses to put the card back on top or into their graveyard
///   (reuses WaitingFor::DigChoice with keep_count=1).
///
/// The exploring creature defaults to the ability's source_id.
/// If the ability has a targeted object, that creature explores instead.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine the exploring creature
    let explorer_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .unwrap_or(ability.source_id);

    // CR 701.44a + CR 614.1a + CR 614.5: Consult explore replacements (Twists and
    // Turns, Topography Tracker, …) before the reveal/counter/land logic runs.
    // Seed the proposed event's applied set from the resolving ability so an
    // explore produced by a doubling replacement's execute chain excludes the
    // replacement that produced it (no self-invocation) while remaining open to
    // other doublers (CR 616.1f). Empty for a first-time explore.
    let proposed = ProposedEvent::Explore {
        object_id: explorer_id,
        applied: ability.replacement_applied.clone(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(_) => {}
        ReplacementResult::Prevented => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    resolve_explore_effect(state, ability, explorer_id, events)
}

/// CR 701.44a: Run the explore reveal/counter/land pipeline without consulting
/// replacement effects. Used when a replacement effect's "instead" chain
/// already resolved the replacement (nested explores must not re-enter).
pub(crate) fn resolve_explore_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    explorer_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let controller = state
        .objects
        .get(&explorer_id)
        .map(|object| object.controller)
        .unwrap_or(ability.controller);

    // Find the controller's library
    let player = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .ok_or(EffectError::PlayerNotFound)?;

    if player.library.is_empty() {
        // CR 701.44a: Explore with empty library — just put a +1/+1 counter.
        add_explore_counter(state, explorer_id, events);

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: explorer_id,
            subject: None,
        });
        return Ok(());
    }

    // Reveal top card
    let top_card_id = player.library[0];
    let revealed_name = state
        .objects
        .get(&top_card_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    events.push(GameEvent::CardsRevealed {
        player: controller,
        card_ids: vec![top_card_id],
        card_names: vec![revealed_name],
    });

    // Check if it's a land
    let is_land = state
        .objects
        .get(&top_card_id)
        .map(|obj| obj.card_types.core_types.contains(&CoreType::Land))
        .unwrap_or(false);

    if is_land {
        // CR 701.44a + CR 614.1 + CR 616.1: The revealed land's Library→Hand
        // instruction is a replaceable zone-change event. Keep Explore's
        // completion event on the typed batch tail so a replacement-ordering
        // choice settles the move before "whenever [a creature] explores"
        // triggers or a chained continuation can run.
        let result = zone_pipeline::move_objects_simultaneously_then(
            state,
            vec![ZoneMoveRequest::effect(
                top_card_id,
                crate::types::zones::Zone::Hand,
                ability.source_id,
            )],
            Some(BatchCompletion::ExploreLandDeliveryComplete { explorer_id }),
            events,
        );
        if matches!(result, BatchMoveResult::NeedsChoice) {
            return Ok(());
        }
    } else {
        // CR 701.44a: Nonland revealed — put a +1/+1 counter on the creature,
        // then player chooses to put the card back on top or into graveyard.
        add_explore_counter(state, explorer_id, events);

        // CR 701.44a: the player may put the revealed nonland card back on top
        // of their library, or put it into their graveyard. Model with
        // DigChoice keep_count=1, up_to=true so the player may keep 0 or 1:
        //   - keep 1 -> kept_destination (top of library, "put it back")
        //   - keep 0 -> rest_destination (graveyard)
        // The card must NEVER go to hand for a nonland explore.
        state.waiting_for = WaitingFor::DigChoice {
            player: controller,
            library_owner: controller,
            selectable_cards: vec![top_card_id],
            cards: vec![top_card_id],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(crate::types::zones::Zone::Library),
            rest_destination: Some(crate::types::zones::Zone::Graveyard),
            source_id: Some(ability.source_id),
            enter_tapped: false,
        };

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: explorer_id,
            subject: None,
        });
    }

    Ok(())
}

/// CR 701.44a + CR 701.44b + CR 614.1 + CR 616.1: A revealed land's proposed
/// hand delivery has settled. The permanent explored even when a replacement
/// redirected the land elsewhere, so emit its completion event exactly once;
/// the replacement-resume boundary then drains any already-parked continuation.
pub(crate) fn complete_land_delivery(
    explorer_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Explore,
        source_id: explorer_id,
        subject: None,
    });
    BatchMoveResult::Done
}

/// CR 701.44d: If multiple permanents explore simultaneously, controllers choose
/// the order within APNAP buckets and each permanent explores one at a time.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExploreAll { filter } = &ability.effect else {
        return Ok(());
    };
    let remaining = collect_explorers(state, ability, filter);
    advance_explore_all(state, ability, remaining, events)
}

pub fn handle_choice(
    state: &mut GameState,
    chosen: ObjectId,
    remaining: &[ObjectId],
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, crate::game::engine::EngineError> {
    let WaitingFor::ExploreChoice { choosable, .. } = &state.waiting_for else {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Not waiting for explore choice".to_string(),
        ));
    };
    if !choosable.contains(&chosen) {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Invalid explore choice".to_string(),
        ));
    }

    let remaining: Vec<ObjectId> = remaining
        .iter()
        .copied()
        .filter(|obj_id| *obj_id != chosen)
        .collect();
    resolve_single_explorer(state, ability, chosen, remaining, events).map_err(|err| {
        crate::game::engine::EngineError::InvalidAction(format!(
            "Failed to continue explore sequence: {err}"
        ))
    })?;
    Ok(state.waiting_for.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, ReplacementDefinition,
        TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_explore_ability(source_id: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Explore, vec![], source_id, PlayerId(0))
    }

    /// Build a creature carrying Topography Tracker's explore replacement:
    /// "If a creature you control would explore, instead it explores, then it
    /// explores again." (execute = Explore, sub_ability = Explore).
    fn topography_replacement() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Explore)
            .execute(
                AbilityDefinition::new(AbilityKind::Spell, Effect::Explore)
                    .sub_ability(AbilityDefinition::new(AbilityKind::Spell, Effect::Explore)),
            )
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
    }

    /// CR 614.5 + CR 701.44a (issue #5272): Topography Tracker doubles an
    /// explore. The two explores must resolve SEQUENTIALLY through the
    /// interactive pipeline — the first explore reveals a nonland and pauses on
    /// its own DigChoice; the second explore resumes only AFTER that choice.
    /// The naive back-to-back applier revealed the same top card twice (the
    /// first DigChoice never moved it), granting two counters up front and
    /// clobbering the first choice. This test locks correct sequencing.
    #[test]
    fn topography_double_explore_sequences_two_distinct_reveals() {
        let mut state = GameState::new_two_player(42);

        // Tracker carries the double-explore replacement.
        let tracker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Topography Tracker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&tracker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&tracker)
            .unwrap()
            .replacement_definitions
            .push(topography_replacement());

        // The exploring creature.
        let explorer = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Explorer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Three DISTINCT nonlands on top (A, B, C) so both a re-reveal of the
        // same top card AND a spurious self-re-double are detectable, with a land
        // beneath to anchor the library top. Card C must stay untouched: the two
        // explores consume A and B, and the producing replacement must NOT apply
        // to its own output (CR 614.5), so C is never revealed.
        let bolt_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        let shock_b = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Library,
        );
        let spark_c = create_object(
            &mut state,
            CardId(6),
            PlayerId(0),
            "Spark".to_string(),
            Zone::Library,
        );
        let land = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        for id in [bolt_a, shock_b, spark_c] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Instant);
        }
        state.players[0].library = vec![bolt_a, shock_b, spark_c, land].into();

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // First explore only: exactly ONE counter so far, paused on card A's
        // DigChoice, with the second explore stashed as a continuation.
        assert_eq!(
            state.objects[&explorer]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "only the first of the two explores should have resolved before the DigChoice"
        );
        match &state.waiting_for {
            WaitingFor::DigChoice { cards, .. } => {
                assert_eq!(
                    cards.as_slice(),
                    &[bolt_a],
                    "first explore must reveal the current top card (A)"
                );
            }
            other => panic!("expected DigChoice from first explore, got {other:?}"),
        }
        assert!(
            state.active_ability_continuation().is_some(),
            "the second explore must be stashed while the first waits on DigChoice"
        );

        // Resolve the first DigChoice: send card A to the graveyard so the
        // second explore reveals a DIFFERENT card (B).
        engine::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();

        assert_eq!(
            state.objects[&explorer]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "second explore should add another counter after the first choice resolves"
        );
        assert!(
            state.players[0].graveyard.contains(&bolt_a),
            "declined first-explore card A must be in the graveyard"
        );
        match &state.waiting_for {
            WaitingFor::DigChoice { cards, .. } => {
                assert_eq!(
                    cards.as_slice(),
                    &[shock_b],
                    "second explore must reveal the NEW top card (B), not re-reveal A"
                );
            }
            other => panic!("expected DigChoice from second explore, got {other:?}"),
        }

        // CR 614.5: resolving the second explore's DigChoice must TERMINATE the
        // explore — the producing replacement cannot apply to its own output, so
        // there is no third explore. If the second link had lost its applied-set
        // exclusion and re-doubled, card C would be revealed here for a third
        // counter and a fresh DigChoice.
        engine::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();

        assert_eq!(
            state.objects[&explorer]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "exactly two explores must occur — the doubler must not re-apply to itself (CR 614.5)"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
            "the doubled explore must terminate, not spawn a third DigChoice"
        );
        assert_eq!(
            state.players[0].library.front().copied(),
            Some(spark_c),
            "card C must be untouched on top — the doubler never explores a third time"
        );
    }

    /// CR 616.1 + CR 701.44a (issue #5272): when a multi-creature explore
    /// ("each creature you control explores") is doubled, one creature's two
    /// explores must stay CONSECUTIVE — the next creature must not explore
    /// between them. Models the `Explore(C1).sub(Explore(C2))` chain that
    /// `resolve_single_explorer` builds (Hakbal of the Surging Soul), with
    /// Topography Tracker doubling each explore. After C1's first DigChoice
    /// resolves, C1 must already have its SECOND counter while C2 has none.
    #[test]
    fn doubled_explore_keeps_one_creatures_two_explores_consecutive() {
        let mut state = GameState::new_two_player(42);

        let tracker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Topography Tracker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&tracker)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&tracker)
            .unwrap()
            .replacement_definitions
            .push(topography_replacement());

        let c1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "First".to_string(),
            Zone::Battlefield,
        );
        let c2 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second".to_string(),
            Zone::Battlefield,
        );
        for id in [c1, c2] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        // Nonlands on top so each explore pauses on a DigChoice; a land anchors
        // the bottom.
        let mut lib = Vec::new();
        for cid in 10..14 {
            let card = create_object(
                &mut state,
                CardId(cid),
                PlayerId(0),
                format!("Nonland {cid}"),
                Zone::Library,
            );
            state
                .objects
                .get_mut(&card)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Instant);
            lib.push(card);
        }
        state.players[0].library = lib.into();

        // Explore C1, then (as the printed next instruction) explore C2 — the
        // shape `resolve_single_explorer` produces for a two-creature explore.
        let second = ResolvedAbility::new(
            Effect::Explore,
            vec![TargetRef::Object(c2)],
            c1,
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Explore,
            vec![TargetRef::Object(c1)],
            c1,
            PlayerId(0),
        )
        .sub_ability(second);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // C1's first explore paused on its DigChoice; C1 has one counter, C2 none.
        assert_eq!(
            state.objects[&c1]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
        );
        assert_eq!(
            state.objects[&c2].counters.get(&CounterType::Plus1Plus1),
            None
        );

        // Resolve C1's first DigChoice. C1's SECOND explore must run next — NOT
        // C2's — so C1 reaches two counters while C2 still has none.
        engine::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();

        assert_eq!(
            state.objects[&c1]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "C1's two explores must be consecutive — C2 must not interleave between them"
        );
        assert_eq!(
            state.objects[&c2].counters.get(&CounterType::Plus1Plus1),
            None,
            "C2 must not have explored until C1's doubled explore fully completes"
        );

        // Drain C1's second DigChoice: the appended C2 sub must now actually
        // fire (its own doubled explore begins), proving the sub was queued
        // AFTER C1's pair rather than dropped.
        engine::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();
        assert_eq!(
            state.objects[&c1]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "C1 must not explore a third time after its pair completes"
        );
        assert_eq!(
            state.objects[&c2]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "C2's explore must run once C1's doubled explore has fully resolved"
        );
    }

    #[test]
    fn explore_scry_prelude_replacement_runs_before_explore() {
        let mut state = GameState::new_two_player(42);

        let twists = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Twists and Turns".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&twists)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let replacement = ReplacementDefinition::new(ReplacementEvent::Explore)
            .execute(
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Scry {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .sub_ability(AbilityDefinition::new(AbilityKind::Spell, Effect::Explore)),
            )
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ));
        state
            .objects
            .get_mut(&twists)
            .unwrap()
            .replacement_definitions
            .push(replacement);

        let explorer = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Explorer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let top_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&top_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 614.5 + CR 701.44a: "scry 1, THEN it explores" resolves in printed
        // order through the interactive pipeline. The scry parks its choice FIRST;
        // the explore is stashed and must not run yet (no counter, no reveal).
        match &state.waiting_for {
            WaitingFor::ScryChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.as_slice(), &[top_card]);
            }
            other => panic!("expected the scry prelude to pause first, got {other:?}"),
        }
        assert!(
            !state.objects[&explorer]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "the explore must not run until the scry choice resolves"
        );
        assert!(
            state.active_ability_continuation().is_some(),
            "the explore link must be stashed while the scry waits for a choice"
        );

        // Keep the card on top; the stashed explore now runs and reveals it.
        engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![top_card],
            },
        )
        .unwrap();

        assert_eq!(
            state.objects[&explorer]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "the scry prelude must still leave the creature exploring (+1/+1 counter)"
        );
        match &state.waiting_for {
            WaitingFor::DigChoice { cards, .. } => {
                assert_eq!(cards.as_slice(), &[top_card]);
            }
            other => panic!("expected the explore's nonland DigChoice, got {other:?}"),
        }
    }

    #[test]
    fn test_explore_land_goes_to_hand() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jadelight Ranger".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&explorer).unwrap().power = Some(2);
        state.objects.get_mut(&explorer).unwrap().toughness = Some(1);
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Put a land on top of library
        let land_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.44a: Land revealed — no counter on explorer
        assert!(!state.objects[&explorer]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        // Land moved to hand
        assert!(state.players[0].hand.contains(&land_id));
        // Land removed from library
        assert!(!state.players[0].library.contains(&land_id));
    }

    #[test]
    fn test_explore_nonland_adds_counter_and_gives_choice() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Merfolk Branchwalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Put a nonland on top of library
        let spell_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.44a: Nonland revealed — explorer gets +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );

        // Player chooses to put card back on top or into graveyard
        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0], spell_id);
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// Build an explorer + a nonland on top of its controller's library, with a
    /// land beneath it so "top of library" is unambiguous. Returns (state, ability,
    /// explorer, nonland_id).
    fn nonland_explore_setup() -> (GameState, ResolvedAbility, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Merfolk Branchwalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let spell_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        // A land beneath the revealed nonland so library-top is meaningful.
        let beneath = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Island".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&beneath)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let ability = make_explore_ability(explorer);
        (state, ability, explorer, spell_id)
    }

    /// CR 701.44a: a put-back explored nonland card returns to the TOP of the
    /// library, never to hand. Regression for #2017 / #2005 (the kept card
    /// previously fell through to Zone::Hand).
    #[test]
    fn explore_nonland_put_back_goes_to_library_top_not_hand() {
        let (mut state, ability, _explorer, spell_id) = nonland_explore_setup();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Keep the revealed card (put it back on top of the library).
        let waiting = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            crate::types::actions::GameAction::SelectCards {
                cards: vec![spell_id],
            },
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].library.front().copied(),
            Some(spell_id),
            "put-back explored nonland must be on top of the library"
        );
        assert!(
            !state.players[0].hand.contains(&spell_id),
            "explored nonland must never go to hand"
        );
    }

    /// CR 701.44a: declining to put the card back sends the explored nonland to
    /// the graveyard (not hand). Regression for #2017 — `up_to: false` previously
    /// forced keeping the card, removing the graveyard option entirely.
    #[test]
    fn explore_nonland_decline_goes_to_graveyard() {
        let (mut state, ability, _explorer, spell_id) = nonland_explore_setup();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Decline (keep 0) — put it into the graveyard.
        let waiting = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting,
            crate::types::actions::GameAction::SelectCards { cards: vec![] },
            &mut events,
        )
        .unwrap();

        assert!(
            state.players[0].graveyard.contains(&spell_id),
            "declined explored nonland must go to graveyard"
        );
        assert!(
            !state.players[0].hand.contains(&spell_id),
            "explored nonland must never go to hand"
        );
        assert!(
            !state.players[0].library.contains(&spell_id),
            "declined explored nonland must leave the library"
        );
    }

    #[test]
    fn test_explore_empty_library_adds_counter() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Explorer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(state.players[0].library.is_empty());

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // With empty library, explorer still gets +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );
    }

    #[test]
    fn targeted_explore_uses_target_controllers_library() {
        let mut state = GameState::new_two_player(42);
        let target = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opponent_top = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opponent Spell".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&opponent_top)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        let _controller_top = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Controller Land".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Explore,
            vec![TargetRef::Object(target)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Explore,
                    source_id
                , ..} if *source_id == target
            )),
            "explore completion event should identify the exploring permanent"
        );
        assert_eq!(
            state.objects[&target].counters[&CounterType::Plus1Plus1],
            1,
            "targeted explore should put the counter on the chosen creature"
        );
        match &state.waiting_for {
            WaitingFor::DigChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(cards.as_slice(), &[opponent_top]);
            }
            other => panic!("expected DigChoice from opponent library, got {other:?}"),
        }
    }

    #[test]
    fn explore_all_waits_for_choice_when_one_player_has_multiple_explorers() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "First".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Second".to_string(),
            Zone::Battlefield,
        );
        for creature in [first, second] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let ability = ResolvedAbility::new(
            Effect::ExploreAll {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            vec![],
            ObjectId(901),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ExploreChoice {
                player,
                choosable,
                remaining,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(choosable.len(), 2);
                assert!(choosable.contains(&first));
                assert!(choosable.contains(&second));
                assert_eq!(remaining.len(), 2);
            }
            other => panic!("expected ExploreChoice, got {other:?}"),
        }
    }

    #[test]
    fn explore_all_parent_target_uses_inherited_targets() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "First".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Second".to_string(),
            Zone::Battlefield,
        );
        let outsider = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Outsider".to_string(),
            Zone::Battlefield,
        );
        for creature in [first, second, outsider] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::ExploreAll {
                filter: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            ObjectId(902),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ExploreChoice { choosable, .. } => {
                assert_eq!(choosable.len(), 2);
                assert!(choosable.contains(&first));
                assert!(choosable.contains(&second));
                assert!(!choosable.contains(&outsider));
            }
            other => panic!("expected ExploreChoice, got {other:?}"),
        }
    }

    /// CR 701.44a (issue #1151): Jadelight Ranger explores twice — the second
    /// explore must resume after the first explore's nonland DigChoice completes.
    #[test]
    fn chained_explore_resumes_after_nonland_dig_choice() {
        let mut state = GameState::new_two_player(42);
        let ranger = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jadelight Ranger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&ranger)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bolt_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        let bolt_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Library,
        );
        state.players[0].library = vec![bolt_a, bolt_b].into();

        let second_explore = ResolvedAbility::new(Effect::Explore, vec![], ranger, PlayerId(0));
        let ability = ResolvedAbility::new(Effect::Explore, vec![], ranger, PlayerId(0))
            .sub_ability(second_explore);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
            "first explore should pause on DigChoice, got {:?}",
            state.waiting_for
        );
        assert!(
            state.active_ability_continuation().is_some(),
            "second explore must be stashed while first explore waits for DigChoice"
        );
        assert_eq!(
            state.objects[&ranger]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "first explore should add one +1/+1 counter"
        );

        let WaitingFor::DigChoice { cards, .. } = state.waiting_for.clone() else {
            unreachable!();
        };
        engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![cards[0]],
            },
        )
        .unwrap();

        assert_eq!(
            state.objects[&ranger]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "second explore should add another +1/+1 counter after DigChoice resolves"
        );
    }
}

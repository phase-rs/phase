use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::counter::{
    has_positive_counters, positive_counter_types, prune_zero_counters, CounterType,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::{Player, PlayerCounterKind, PlayerId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlayerCounterSource {
    Kind(PlayerCounterKind),
    Energy,
}

const PLAYER_COUNTER_KINDS: [PlayerCounterKind; 4] = [
    PlayerCounterKind::Poison,
    PlayerCounterKind::Experience,
    PlayerCounterKind::Rad,
    PlayerCounterKind::Ticket,
];

fn proliferatable_player_counters(player: &Player) -> Vec<PlayerCounterSource> {
    let mut counters: Vec<_> = PLAYER_COUNTER_KINDS
        .into_iter()
        .filter(|kind| player.player_counter(kind) > 0)
        .map(PlayerCounterSource::Kind)
        .collect();
    if player.energy > 0 {
        counters.push(PlayerCounterSource::Energy);
    }
    counters
}

/// CR 701.34a: Proliferate — controller chooses any number of permanents and/or
/// players that already have counters, then gives each another counter of a kind
/// already there. Sets `WaitingFor::ProliferateChoice` for the player to choose.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.34a: Collect eligible permanents (with counters on them).
    let mut eligible: Vec<TargetRef> = state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| has_positive_counters(&obj.counters))
                .unwrap_or(false)
        })
        .map(|id| TargetRef::Object(*id))
        .collect();

    // CR 701.34a + CR 107.14: players with any counter, including energy, are eligible.
    for player in &state.players {
        if !proliferatable_player_counters(player).is_empty() {
            eligible.push(TargetRef::Player(player.id));
        }
    }

    if eligible.is_empty() {
        // Nothing to proliferate — skip choice and resolve immediately.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        // CR 701.34a: Emit player-action event so proliferate triggers fire
        // even when there are no eligible targets.
        events.push(GameEvent::PlayerPerformedAction {
            player_id: ability.controller,
            action: PlayerActionKind::Proliferate,
        });
        return Ok(());
    }

    // Set WaitingFor so the player can choose which to proliferate.
    state.waiting_for = WaitingFor::ProliferateChoice {
        player: ability.controller,
        eligible,
    };

    Ok(())
}

/// CR 701.34a (operation) + CR 122.1: Resolve `Effect::ProliferateTarget` — the
/// forced single-target form ("for each kind of counter on target permanent or
/// player, give that permanent or player another counter of that kind").
///
/// Unlike `resolve` (the chooser-driven `Proliferate`), the target is already
/// fixed in `ability.targets`, so there is no `ProliferateChoice` prompt: it
/// reuses `apply_proliferate` directly on the resolved target(s). It also does
/// NOT emit `PlayerActionKind::Proliferate` — the card spells out the
/// counter-add rather than using the proliferate keyword action, so it must not
/// fire "whenever you proliferate" triggers.
pub fn resolve_target(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    apply_proliferate(state, ability.controller, &ability.targets, events);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    Ok(())
}

/// Apply proliferate to the selected targets — adds one counter of each kind
/// already present. Called from the engine handler after player makes their choice.
pub fn apply_proliferate(
    state: &mut GameState,
    actor: PlayerId,
    selected: &[TargetRef],
    events: &mut Vec<GameEvent>,
) {
    for target in selected {
        match target {
            TargetRef::Object(obj_id) => {
                if let Some(obj) = state.objects.get_mut(obj_id) {
                    prune_zero_counters(&mut obj.counters);
                }

                let counter_types: Vec<CounterType> = state
                    .objects
                    .get(obj_id)
                    .map(|obj| positive_counter_types(&obj.counters))
                    .unwrap_or_default();

                for ct in counter_types {
                    super::counters::apply_counter_addition(state, actor, *obj_id, ct, 1, events);
                }
            }
            TargetRef::Player(pid) => {
                let counters = state
                    .players
                    .iter()
                    .find(|p| p.id == *pid)
                    .map(proliferatable_player_counters)
                    .unwrap_or_default();

                for counter in counters {
                    if let Some(player) = state.players.iter_mut().find(|p| p.id == *pid) {
                        match counter {
                            PlayerCounterSource::Kind(kind) => {
                                player.add_player_counters(&kind, 1);
                                events.push(GameEvent::PlayerCounterChanged {
                                    player: *pid,
                                    counter_kind: kind,
                                    delta: 1,
                                });
                            }
                            PlayerCounterSource::Energy => {
                                player.energy += 1;
                                events.push(GameEvent::EnergyChanged {
                                    player: *pid,
                                    delta: 1,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::Effect;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_proliferate_ability() -> ResolvedAbility {
        ResolvedAbility::new(Effect::Proliferate, vec![], ObjectId(100), PlayerId(0))
    }

    #[test]
    fn resolve_sets_proliferate_choice() {
        let mut state = GameState::new_two_player(42);
        let obj1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj1)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should set WaitingFor::ProliferateChoice with the eligible permanent.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ProliferateChoice { .. }
        ));
        if let WaitingFor::ProliferateChoice { eligible, .. } = &state.waiting_for {
            assert_eq!(eligible.len(), 1);
            assert!(matches!(eligible[0], TargetRef::Object(id) if id == obj1));
        }
    }

    #[test]
    fn resolve_skips_choice_when_no_eligible() {
        let mut state = GameState::new_two_player(42);
        // No permanents with counters.
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Empty".to_string(),
            Zone::Battlefield,
        );

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should resolve immediately with EffectResolved event.
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn apply_proliferate_adds_counters() {
        let mut state = GameState::new_two_player(42);
        let obj1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj1)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        let mut events = Vec::new();
        apply_proliferate(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(obj1)],
            &mut events,
        );

        assert_eq!(state.objects[&obj1].counters[&CounterType::Plus1Plus1], 3);
    }

    #[test]
    fn apply_proliferate_multiple_counter_types() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);

        let mut events = Vec::new();
        apply_proliferate(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(obj)],
            &mut events,
        );

        assert_eq!(state.objects[&obj].counters[&CounterType::Plus1Plus1], 2);
        assert_eq!(
            state.objects[&obj].counters[&CounterType::Generic("charge".to_string())],
            4
        );
    }

    #[test]
    fn apply_proliferate_emits_counter_added_events() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let mut events = Vec::new();
        apply_proliferate(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(obj)],
            &mut events,
        );

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CounterAdded {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
                ..
            }
        )));
    }

    #[test]
    fn proliferate_includes_poisoned_players() {
        let mut state = GameState::new_two_player(42);
        state.players[1].poison_counters = 3;

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        if let WaitingFor::ProliferateChoice { eligible, .. } = &state.waiting_for {
            assert!(eligible
                .iter()
                .any(|t| matches!(t, TargetRef::Player(pid) if *pid == PlayerId(1))));
        } else {
            panic!("Expected ProliferateChoice");
        }
    }

    #[test]
    fn proliferate_includes_players_with_generic_player_counters() {
        let mut state = GameState::new_two_player(42);
        state.players[1].add_player_counters(&PlayerCounterKind::Experience, 2);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        if let WaitingFor::ProliferateChoice { eligible, .. } = &state.waiting_for {
            assert!(eligible
                .iter()
                .any(|t| matches!(t, TargetRef::Player(pid) if *pid == PlayerId(1))));
        } else {
            panic!("Expected ProliferateChoice");
        }
    }

    #[test]
    fn proliferate_includes_players_with_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[1].energy = 2;

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        if let WaitingFor::ProliferateChoice { eligible, .. } = &state.waiting_for {
            assert!(eligible
                .iter()
                .any(|t| matches!(t, TargetRef::Player(pid) if *pid == PlayerId(1))));
        } else {
            panic!("Expected ProliferateChoice");
        }
    }

    #[test]
    fn apply_proliferate_adds_all_player_counter_kinds_and_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[1].poison_counters = 1;
        state.players[1].add_player_counters(&PlayerCounterKind::Experience, 2);
        state.players[1].add_player_counters(&PlayerCounterKind::Rad, 3);
        state.players[1].energy = 4;

        let mut events = Vec::new();
        apply_proliferate(
            &mut state,
            PlayerId(0),
            &[TargetRef::Player(PlayerId(1))],
            &mut events,
        );

        assert_eq!(state.players[1].poison_counters, 2);
        assert_eq!(
            state.players[1].player_counter(&PlayerCounterKind::Experience),
            3
        );
        assert_eq!(state.players[1].player_counter(&PlayerCounterKind::Rad), 4);
        assert_eq!(state.players[1].energy, 5);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerCounterChanged {
                player: PlayerId(1),
                counter_kind: PlayerCounterKind::Poison,
                delta: 1,
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerCounterChanged {
                player: PlayerId(1),
                counter_kind: PlayerCounterKind::Experience,
                delta: 1,
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerCounterChanged {
                player: PlayerId(1),
                counter_kind: PlayerCounterKind::Rad,
                delta: 1,
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EnergyChanged {
                player: PlayerId(1),
                delta: 1,
            }
        )));
    }

    #[test]
    fn resolve_excludes_permanent_with_only_zero_count_entries() {
        let mut state = GameState::new_two_player(42);
        let stale = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Stale Counter Map".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&stale)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 0);

        let pumped = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Still Pumped".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pumped)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        if let WaitingFor::ProliferateChoice { eligible, .. } = &state.waiting_for {
            assert!(
                !eligible
                    .iter()
                    .any(|t| matches!(t, TargetRef::Object(id) if *id == stale)),
                "zero-count stale entry must not make a permanent proliferate-eligible"
            );
            assert!(
                eligible
                    .iter()
                    .any(|t| matches!(t, TargetRef::Object(id) if *id == pumped)),
                "permanent with a positive counter must remain eligible"
            );
        } else {
            panic!("Expected ProliferateChoice");
        }
    }

    #[test]
    fn apply_proliferate_skips_zero_count_counter_types() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mixed Counters".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 0);
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 2);

        let mut events = Vec::new();
        apply_proliferate(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(obj)],
            &mut events,
        );

        assert!(
            !state.objects[&obj]
                .counters
                .contains_key(&CounterType::Plus1Plus1),
            "proliferate must not add a counter type that was at zero"
        );
        assert_eq!(
            state.objects[&obj].counters[&CounterType::Generic("charge".to_string())],
            3
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::CounterAdded {
                    counter_type: CounterType::Plus1Plus1,
                    ..
                }
            )),
            "no CounterAdded event for the zero-count type"
        );
    }

    #[test]
    fn apply_proliferate_after_counter_removal_does_not_restore_lost_type() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        super::super::counters::apply_counter_removal(
            &mut state,
            obj,
            CounterType::Plus1Plus1,
            1,
            &mut Vec::new(),
        );
        assert!(
            state.objects[&obj].counters.is_empty(),
            "removal should prune the last +1/+1 counter"
        );

        let mut events = Vec::new();
        apply_proliferate(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(obj)],
            &mut events,
        );

        assert!(
            state.objects[&obj].counters.is_empty(),
            "proliferate must not add +1/+1 back after the permanent lost its last one"
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, GameEvent::CounterAdded { .. })),
            "no counter should be added"
        );
    }

    #[test]
    fn resolve_skips_choice_when_only_zero_count_permanents_exist() {
        let mut state = GameState::new_two_player(42);
        let stale = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Stale".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&stale)
            .unwrap()
            .counters
            .insert(CounterType::Minus1Minus1, 0);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::ProliferateChoice { .. }),
            "only stale zero-count entries should not open a proliferate choice"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn resolve_target_adds_one_of_each_kind_without_prompt() {
        use crate::types::ability::TargetFilter;

        // CR 701.34a + CR 122.1: Skyship Plunderer — the forced single-target
        // form adds one counter of each kind already present on the chosen
        // target, with no ProliferateChoice prompt.
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let counters = &mut state.objects.get_mut(&obj).unwrap().counters;
            counters.insert(CounterType::Plus1Plus1, 2);
            counters.insert(CounterType::Generic("charge".to_string()), 1);
        }

        let ability = ResolvedAbility::new(
            Effect::ProliferateTarget {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_target(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj].counters[&CounterType::Plus1Plus1], 3);
        assert_eq!(
            state.objects[&obj].counters[&CounterType::Generic("charge".to_string())],
            2
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::ProliferateChoice { .. }),
            "the targeted form must not open a proliferate choice"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn resolve_target_adds_to_targeted_player() {
        use crate::types::ability::TargetFilter;

        // The target pool is "permanent or player": a poisoned player gets one
        // more poison counter.
        let mut state = GameState::new_two_player(42);
        state.players[1].poison_counters = 2;

        let ability = ResolvedAbility::new(
            Effect::ProliferateTarget {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_target(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].poison_counters, 3);
    }
}

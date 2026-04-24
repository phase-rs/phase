use crate::game::mana_sources::mana_color_to_type;
use crate::types::ability::{
    DoubleTarget, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::counter::{parse_counter_type, CounterType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaType, ManaUnit};
use crate::types::player::PlayerId;

/// CR 701.10d-f: Double counters on a permanent, a player's life total, or mana pool.
/// Dispatches on `DoubleTarget` variant.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Double {
        target_kind,
        target,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("expected Double effect".into()));
    };

    match target_kind {
        DoubleTarget::Counters { counter_type } => {
            resolve_double_counters(state, ability, events, target, counter_type.as_deref())
        }
        DoubleTarget::LifeTotal => resolve_double_life(state, ability, events, target),
        DoubleTarget::ManaPool { color } => {
            resolve_double_mana(state, ability, events, target, color.as_ref())
        }
    }
}

/// CR 701.10e: Double the number of a kind of counter (or all kinds) on target permanent(s).
fn resolve_double_counters(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    target: &TargetFilter,
    counter_type: Option<&str>,
) -> Result<(), EffectError> {
    let obj_ids = resolve_object_targets(ability, target);

    for obj_id in obj_ids {
        // Snapshot current counters to avoid borrow issues
        let counters_snapshot: Vec<(crate::types::counter::CounterType, u32)> = {
            let obj = state
                .objects
                .get(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            if let Some(ct_str) = counter_type {
                // CR 701.10e: Double only the specified counter type
                let ct = parse_counter_type(ct_str);
                let count = obj.counters.get(&ct).copied().unwrap_or(0);
                if count > 0 {
                    vec![(ct, count)]
                } else {
                    vec![]
                }
            } else {
                // CR 701.10e: Double each kind of counter on the permanent
                obj.counters
                    .iter()
                    .filter(|(_, &count)| count > 0)
                    .map(|(ct, &count)| (ct.clone(), count))
                    .collect()
            }
        };

        // CR 701.10e: Add N more of each counter type where N = current count
        // Uses direct manipulation (like resolve_multiply) rather than replacement pipeline.
        for (ct, current_count) in counters_snapshot {
            let obj = state
                .objects
                .get_mut(&obj_id)
                .ok_or(EffectError::ObjectNotFound(obj_id))?;
            let entry = obj.counters.entry(ct.clone()).or_insert(0);
            *entry += current_count;

            if matches!(ct, CounterType::Plus1Plus1 | CounterType::Minus1Minus1) {
                state.layers_dirty = true;
            }

            events.push(GameEvent::CounterAdded {
                object_id: obj_id,
                counter_type: ct,
                count: current_count,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Double,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.10d: Double a player's life total.
/// If life > 0: gain life equal to current total (new total = 2x).
/// If life < 0: lose life equal to |current total| (new total = 2x negative).
/// If life == 0: no change.
///
/// Routes the gain/loss through `apply_life_gain` / `apply_damage_life_loss`
/// so the same replacement-pipeline and can't-gain / can't-lose short-circuits
/// that govern all other life-change events apply here too (CR 119.7 + 119.8).
fn resolve_double_life(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    target: &TargetFilter,
) -> Result<(), EffectError> {
    let player_id = resolve_player_target(ability, target);

    let current_life = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?
        .life;

    if current_life > 0 {
        // CR 701.10d: Gain life equal to current total.
        let _ = crate::game::effects::life::apply_life_gain(
            state,
            player_id,
            current_life as u32,
            events,
        );
    } else if current_life < 0 {
        // CR 701.10d: Lose |current_life| additional life so the new total is 2x.
        let _ = crate::game::effects::life::apply_damage_life_loss(
            state,
            player_id,
            (-current_life) as u32,
            events,
        );
    }
    // life == 0: no change.

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Double,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.10f: Double the amount of a type of mana in a player's mana pool.
fn resolve_double_mana(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    target: &TargetFilter,
    color: Option<&ManaColor>,
) -> Result<(), EffectError> {
    let player_id = resolve_player_target(ability, target);

    // Collect the mana types and counts to add
    let mana_to_add: Vec<(ManaType, usize)> = {
        let player = state
            .players
            .iter()
            .find(|p| p.id == player_id)
            .ok_or(EffectError::PlayerNotFound)?;

        if let Some(c) = color {
            let mt = mana_color_to_type(c);
            let count = player.mana_pool.count_color(mt);
            if count > 0 {
                vec![(mt, count)]
            } else {
                vec![]
            }
        } else {
            // All colors
            ManaColor::ALL
                .iter()
                .map(|c| {
                    let mt = mana_color_to_type(c);
                    (mt, player.mana_pool.count_color(mt))
                })
                .filter(|(_, count)| *count > 0)
                .collect()
        }
    };

    // CR 701.10f: Add equal amount of each mana type
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?;

    for (mana_type, count) in mana_to_add {
        for _ in 0..count {
            player.mana_pool.add(ManaUnit {
                color: mana_type,
                source_id: ability.source_id,
                snow: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });

            events.push(GameEvent::ManaAdded {
                player_id,
                mana_type,
                source_id: ability.source_id,
                tapped_for_mana: false,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Double,
        source_id: ability.source_id,
    });

    Ok(())
}

/// Resolve object targets from ability targets or self-ref.
fn resolve_object_targets(ability: &ResolvedAbility, target: &TargetFilter) -> Vec<ObjectId> {
    match target {
        TargetFilter::SelfRef | TargetFilter::None => vec![ability.source_id],
        _ => ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect(),
    }
}

/// Resolve a player target from the ability.
fn resolve_player_target(ability: &ResolvedAbility, target: &TargetFilter) -> PlayerId {
    match target {
        TargetFilter::Controller | TargetFilter::SelfRef => ability.controller,
        _ => ability
            .targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Player(pid) = t {
                    Some(*pid)
                } else {
                    None
                }
            })
            .unwrap_or(ability.controller),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{AbilityKind, SpellContext};
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_double_ability(
        target_kind: DoubleTarget,
        target: TargetFilter,
        controller: PlayerId,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::Double {
                target_kind,
                target,
            },
            controller,
            source_id: ObjectId(100),
            targets,
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            player_scope: None,
            chosen_x: None,
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            description: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
        }
    }

    #[test]
    fn double_counters_specific_type() {
        let mut state = GameState::default();
        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(0),
            PlayerId(0),
            "Test".into(),
            Zone::Battlefield,
        );
        obj.counters.insert(CounterType::Plus1Plus1, 3);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters {
                counter_type: Some("+1/+1".to_string()),
            },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10e: 3 counters doubled → 6 counters
        assert_eq!(
            state.objects[&obj_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            6
        );
    }

    #[test]
    fn double_counters_all_kinds() {
        let mut state = GameState::default();
        let obj_id = ObjectId(1);
        let mut obj = GameObject::new(
            obj_id,
            CardId(0),
            PlayerId(0),
            "Test".into(),
            Zone::Battlefield,
        );
        obj.counters.insert(CounterType::Plus1Plus1, 2);
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 1);
        state.objects.insert(obj_id, obj);
        state.battlefield.push_back(obj_id);

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::Counters { counter_type: None },
            TargetFilter::Any,
            PlayerId(0),
            vec![TargetRef::Object(obj_id)],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10e: 2 +1/+1 → 4, 1 charge → 2
        let obj = &state.objects[&obj_id];
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            4
        );
        assert_eq!(
            obj.counters
                .get(&CounterType::Generic("charge".to_string()))
                .copied()
                .unwrap_or(0),
            2
        );
    }

    #[test]
    fn double_life_total() {
        let mut state = GameState::default();
        // Set player 0's life to 15
        state.players[0].life = 15;

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::LifeTotal,
            TargetFilter::Controller,
            PlayerId(0),
            vec![],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10d: 15 life → 30 life
        assert_eq!(state.players[0].life, 30);
    }

    /// CR 701.10d + CR 119.7: Doubling life routes through `apply_life_gain`, so
    /// a CantGainLife static on the affected player suppresses the doubling.
    #[test]
    fn double_life_total_blocked_by_cant_gain_life() {
        use crate::game::zones::create_object;
        use crate::types::ability::{ControllerRef, StaticDefinition, TypedFilter};
        use crate::types::identifiers::CardId;
        use crate::types::statics::StaticMode;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 15;

        // Attach a CantGainLife static affecting PlayerId(0).
        let lock_id = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantGainLife).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::LifeTotal,
            TargetFilter::Controller,
            PlayerId(0),
            vec![],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // Life total must be unchanged — the Double effect's life-gain half is
        // short-circuited by the CantGainLife lock before the pipeline runs.
        assert_eq!(state.players[0].life, 15);
    }

    #[test]
    fn double_mana_pool() {
        let mut state = GameState::default();
        // Add 3 red mana to player 0's pool
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Red,
                source_id: ObjectId(50),
                snow: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }

        let mut events = Vec::new();
        let ability = make_double_ability(
            DoubleTarget::ManaPool {
                color: Some(ManaColor::Red),
            },
            TargetFilter::Controller,
            PlayerId(0),
            vec![],
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.10f: 3 red → 6 red
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 6);
    }
}

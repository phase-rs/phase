use crate::types::ability::{AbilityCost, AbilityDefinition, Effect, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ManaAbilityResume, PendingManaAbility, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects::mana::resolve_mana_types;
use super::engine::EngineError;
use super::filter::{matches_target_filter, FilterContext};
use super::mana_payment;
use super::mana_sources;
use super::sacrifice;

/// Check if a typed ability definition represents a mana ability (CR 605).
/// CR 605.3: Mana abilities produce mana and resolve immediately without using the stack.
/// CR 605.1a: A mana ability cannot have targets. If the effect produces mana but the
/// ability has targeting (e.g., via `multi_target`), it must use the stack instead.
/// Currently `Effect::Mana` has no embedded target field and no `AbilityCost` variant
/// implies targeting, so this check is defensive — if future variants introduce
/// targeting on mana-producing abilities, this guard ensures correctness.
pub fn is_mana_ability(ability_def: &AbilityDefinition) -> bool {
    if !matches!(*ability_def.effect, Effect::Mana { .. }) {
        return false;
    }
    // CR 605.1a: A targeted mana-producing ability is not a mana ability.
    // multi_target is the explicit targeting mechanism on AbilityDefinition.
    ability_def.multi_target.is_none()
}

/// CR 605.2: Mana abilities don't use the stack — they can't be targeted, countered, or responded to.
/// CR 605.3b: Mana abilities resolve immediately when activated.
///
/// Pays the full ability cost (tap, sacrifice, etc.) via `pay_mana_ability_cost`,
/// then produces mana. When `color_override` is `Some`, produces exactly that color
/// instead of resolving the production descriptor — used by auto-tap to pick a
/// specific color for `AnyOneColor` sources (Treasures, etc.).
pub fn resolve_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ManaType>,
) -> Result<(), EngineError> {
    // Pay the full ability cost (tap, sacrifice, etc.)
    pay_mana_ability_cost(state, source_id, player, &ability_def.cost, events)?;

    // Produce mana — resolve the full count from the production descriptor,
    // then apply color_override if present. This ensures dynamic-count producers
    // (e.g., Priest of Titania: {G} per elf) produce the correct amount even
    // when auto-tap specifies a color override.
    let produced_mana = match &*ability_def.effect {
        Effect::Mana { produced, .. } => {
            let resolved = resolve_mana_types(produced, &*state, player, source_id);
            match color_override {
                Some(color) => vec![color; resolved.len()],
                None => resolved,
            }
        }
        _ => Vec::new(),
    };

    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for mana_type in produced_mana {
        mana_payment::produce_mana(state, source_id, mana_type, player, tapped, events);
    }

    Ok(())
}

/// CR 605.3b: Mana abilities resolve immediately unless paying the cost requires a choice.
#[allow(clippy::too_many_arguments)]
pub fn activate_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    resume: ManaAbilityResume,
    color_override: Option<ManaType>,
) -> Result<WaitingFor, EngineError> {
    if let Some((count, creatures)) =
        tap_creature_cost_choice(state, player, source_id, &ability_def.cost)
    {
        if creatures.len() < count {
            return Err(EngineError::ActionNotAllowed(
                "Not enough untapped creatures to pay mana ability cost".to_string(),
            ));
        }
        return Ok(WaitingFor::TapCreaturesForManaAbility {
            player,
            count,
            creatures,
            pending_mana_ability: Box::new(PendingManaAbility {
                player,
                source_id,
                ability_index,
                color_override,
                resume,
            }),
        });
    }

    resolve_mana_ability(
        state,
        source_id,
        player,
        ability_def,
        events,
        color_override,
    )?;
    Ok(resume_waiting_for(player, resume))
}

/// CR 118.3 / CR 605.3b: Complete the tapped-creature choice, then resolve the mana ability.
pub fn handle_tap_creatures_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_creatures: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must tap exactly {} creature(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for mana ability cost".to_string(),
            ));
        }
    }

    let ability_def = state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| obj.abilities.get(pending.ability_index))
        .cloned()
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))?;

    resolve_mana_ability_with_tapped_creatures(
        state,
        pending.source_id,
        pending.player,
        &ability_def,
        events,
        pending.color_override,
        chosen,
    )?;

    Ok(resume_waiting_for(pending.player, pending.resume.clone()))
}

pub fn can_activate_mana_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_def: &AbilityDefinition,
) -> bool {
    // CR 701.35a: Detained permanents' activated abilities can't be activated
    // (mana abilities are activated abilities).
    if state
        .objects
        .get(&source_id)
        .is_some_and(|obj| !obj.detained_by.is_empty())
    {
        return false;
    }
    if let Some((count, creatures)) =
        tap_creature_cost_choice(state, player, source_id, &ability_def.cost)
    {
        return creatures.len() >= count;
    }

    let mut simulated = state.clone();
    resolve_mana_ability(
        &mut simulated,
        source_id,
        player,
        ability_def,
        &mut Vec::new(),
        None,
    )
    .is_ok()
}

/// Pay the full cost of a mana ability. This is the single authority for mana ability
/// cost resolution — callers dispatch activation, they never inspect individual cost
/// components. Handles `Tap`, `Composite { Tap, Sacrifice }`, and future cost variants.
fn pay_mana_ability_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_mana_ability_cost_with_choices(
        state,
        source_id,
        player,
        cost,
        events,
        &mut std::iter::empty(),
    )
}

fn resolve_mana_ability_with_tapped_creatures(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ManaType>,
    tapped_creatures: &[ObjectId],
) -> Result<(), EngineError> {
    let mut chosen = tapped_creatures.iter().copied();
    pay_mana_ability_cost_with_choices(
        state,
        source_id,
        player,
        &ability_def.cost,
        events,
        &mut chosen,
    )?;
    if chosen.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many creatures selected for mana ability cost".to_string(),
        ));
    }

    let produced_mana = match &*ability_def.effect {
        Effect::Mana { produced, .. } => {
            let resolved = resolve_mana_types(produced, &*state, player, source_id);
            match color_override {
                Some(color) => vec![color; resolved.len()],
                None => resolved,
            }
        }
        _ => Vec::new(),
    };

    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for mana_type in produced_mana {
        mana_payment::produce_mana(state, source_id, mana_type, player, tapped, events);
    }

    Ok(())
}

fn pay_mana_ability_cost_with_choices<I>(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
    chosen_tappers: &mut I,
) -> Result<(), EngineError>
where
    I: Iterator<Item = ObjectId>,
{
    match cost {
        Some(AbilityCost::Tap) => tap_source(state, source_id, events)?,
        Some(AbilityCost::PayLife { amount }) => pay_life_cost(state, player, *amount, events)?,
        Some(AbilityCost::TapCreatures { count, filter }) => {
            for _ in 0..*count {
                let chosen_id = chosen_tappers.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing tapped creature selection for mana ability".to_string(),
                    )
                })?;
                tap_selected_creature_for_mana_cost(
                    state,
                    source_id,
                    player,
                    chosen_id,
                    filter,
                    cost_has_source_tap_component(cost),
                    events,
                )?;
            }
        }
        Some(AbilityCost::Composite { costs }) => {
            let exclude_source = costs
                .iter()
                .any(|sub_cost| matches!(sub_cost, AbilityCost::Tap));
            for sub_cost in costs {
                match sub_cost {
                    AbilityCost::Tap => tap_source(state, source_id, events)?,
                    AbilityCost::PayLife { amount } => {
                        pay_life_cost(state, player, *amount, events)?
                    }
                    AbilityCost::TapCreatures { count, filter } => {
                        for _ in 0..*count {
                            let chosen_id = chosen_tappers.next().ok_or_else(|| {
                                EngineError::InvalidAction(
                                    "Missing tapped creature selection for mana ability"
                                        .to_string(),
                                )
                            })?;
                            tap_selected_creature_for_mana_cost(
                                state,
                                source_id,
                                player,
                                chosen_id,
                                filter,
                                exclude_source,
                                events,
                            )?;
                        }
                    }
                    AbilityCost::Sacrifice {
                        target: TargetFilter::SelfRef,
                        ..
                    } => {
                        let _ = sacrifice::sacrifice_permanent(state, source_id, player, events)?;
                    }
                    other => {
                        return Err(EngineError::InvalidAction(format!(
                            "Unsupported mana ability sub-cost: {other:?}"
                        )));
                    }
                }
            }
        }
        Some(other) => {
            return Err(EngineError::InvalidAction(format!(
                "Unsupported mana ability cost: {other:?}"
            )));
        }
        None => {}
    }

    Ok(())
}

fn pay_life_cost(
    state: &mut GameState,
    player: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // CR 118.3 + CR 118.3b: A player can pay life as a cost only if they have enough life,
    // and paying life subtracts that amount from their life total immediately.
    let player_state = &mut state.players[player.0 as usize];
    if player_state.life < amount as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough life to pay mana ability cost".to_string(),
        ));
    }
    player_state.life -= amount as i32;
    events.push(GameEvent::LifeChanged {
        player_id: player,
        amount: -(amount as i32),
    });
    Ok(())
}

/// Tap a permanent as part of paying a mana ability cost.
fn tap_source(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.tapped {
        return Err(EngineError::ActionNotAllowed(
            "Cannot activate tap ability: permanent is tapped".to_string(),
        ));
    }
    let obj = state.objects.get_mut(&source_id).unwrap();
    obj.tapped = true;
    events.push(GameEvent::PermanentTapped {
        object_id: source_id,
        caused_by: None,
    });
    Ok(())
}

fn tap_creature_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (count, filter) = find_tap_creatures_cost(cost.as_ref()?)?;
    let creatures = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if cost_has_source_tap_component(cost) && id == source_id {
                return false;
            }
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.zone != Zone::Battlefield || obj.controller != player || obj.tapped {
                return false;
            }
            matches_target_filter(
                state,
                id,
                filter,
                &FilterContext::from_source(state, source_id),
            )
        })
        .collect();
    Some((count as usize, creatures))
}

fn find_tap_creatures_cost(cost: &AbilityCost) -> Option<(u32, &TargetFilter)> {
    match cost {
        AbilityCost::TapCreatures { count, filter } => Some((*count, filter)),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_tap_creatures_cost),
        _ => None,
    }
}

fn tap_selected_creature_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: &TargetFilter,
    exclude_source: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if exclude_source && chosen_id == source_id {
        return Err(EngineError::ActionNotAllowed(
            "Source cannot satisfy both tap costs".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&chosen_id)
        .ok_or_else(|| EngineError::InvalidAction("Selected creature not found".to_string()))?;
    if obj.zone != Zone::Battlefield || obj.controller != player || obj.tapped {
        return Err(EngineError::ActionNotAllowed(
            "Selected creature is not an untapped creature you control".to_string(),
        ));
    }
    if !matches_target_filter(
        state,
        chosen_id,
        filter,
        &FilterContext::from_source(state, source_id),
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Selected creature does not satisfy mana ability cost".to_string(),
        ));
    }

    state.objects.get_mut(&chosen_id).unwrap().tapped = true;
    events.push(GameEvent::PermanentTapped {
        object_id: chosen_id,
        caused_by: None,
    });
    Ok(())
}

fn cost_has_source_tap_component(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Tap) => true,
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|cost| matches!(cost, AbilityCost::Tap))
        }
        _ => false,
    }
}

fn resume_waiting_for(player: PlayerId, resume: ManaAbilityResume) -> WaitingFor {
    match resume {
        ManaAbilityResume::Priority => WaitingFor::Priority { player },
        ManaAbilityResume::ManaPayment { convoke_mode } => WaitingFor::ManaPayment {
            player,
            convoke_mode,
        },
        ManaAbilityResume::UnlessPayment {
            cost,
            pending_effect,
            effect_description,
        } => WaitingFor::UnlessPayment {
            player,
            cost,
            pending_effect,
            effect_description,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityKind, Effect, ManaProduction, MultiTargetSpec, QuantityExpr,
        TargetFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaType};
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    #[test]
    fn mana_api_type_detected_as_mana_ability() {
        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
        });
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn non_mana_api_type_not_detected() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )
        .cost(AbilityCost::Tap);
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn targeted_mana_producing_ability_is_not_mana_ability() {
        // CR 605.1a: If a mana-producing ability has targets, it must use the stack.
        let mut def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
        });
        def.multi_target = Some(MultiTargetSpec {
            min: 1,
            max: Some(1),
        });
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn draw_ability_is_not_mana_ability() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
        )
        .cost(AbilityCost::Tap);
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn resolve_mana_ability_produces_mana_and_taps() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
        });
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert!(state.objects.get(&obj_id).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn resolve_mana_ability_fails_if_already_tapped() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
        });
        let mut events = Vec::new();
        let result = resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None);

        assert!(result.is_err());
    }

    #[test]
    fn resolve_mana_ability_colorless_produced() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Tap);
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    #[test]
    fn resolve_mana_ability_fixed_multi_color_produces_each_unit() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hybrid Source".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::White, ManaColor::Blue],
        });
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn resolve_composite_cost_taps_and_sacrifices() {
        // CR 111.10a + CR 605.3b: Treasure — Composite {Tap, Sacrifice} mana ability
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        // Mana was produced
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        // Object was sacrificed (moved out of battlefield)
        let obj = state.objects.get(&obj_id);
        assert!(
            obj.is_none() || obj.unwrap().zone != Zone::Battlefield,
            "Treasure should be sacrificed (removed from battlefield)"
        );
        // Events include both tap and sacrifice
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    #[test]
    fn resolve_composite_cost_taps_pays_life_and_produces_mana() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::White, ManaColor::Blue],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::PayLife { amount: 1 }],
        });

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            &def,
            &mut events,
            Some(ManaType::Blue),
        )
        .unwrap();

        assert!(state.objects.get(&obj_id).unwrap().tapped);
        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::LifeChanged {
                player_id,
                amount: -1,
            } if *player_id == PlayerId(0)
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn color_override_produces_specified_color() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Any Color Source".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::White, ManaColor::Blue, ManaColor::Black],
        });
        let mut events = Vec::new();
        // Override to produce Black specifically
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            &def,
            &mut events,
            Some(ManaType::Black),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }
}

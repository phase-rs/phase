use crate::game::zones;
use crate::types::ability::{
    BounceSelection, ControllerRef, Effect, EffectError, EffectKind, FilterProp, ResolvedAbility,
    TargetFilter, TargetRef, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CastingVariant, GameState, StackEntryKind, WaitingFor};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 608.2c + CR 608.2d + CR 109.4 (issue #534): Resolve the *selecting
/// player* for a non-targeted graveyard-return `Bounce` whose filter scopes
/// ownership to a chosen player. Walks the filter for the first
/// `FilterProp::Owned { controller: ChosenPlayer { index } }` (and falls back
/// to a top-level `TypedFilter.controller` for symmetry), then indexes
/// `ability.chosen_players` — populated by the preceding `Choose` clause —
/// to recover the concrete `PlayerId`. Returns `None` when the filter is not
/// chosen-scoped, signalling the caller to fall back to `ability.controller`.
fn chosen_player_for_filter(ability: &ResolvedAbility, filter: &TargetFilter) -> Option<PlayerId> {
    fn find_index(filter: &TargetFilter) -> Option<u8> {
        match filter {
            TargetFilter::Typed(tf) => {
                if let Some(ControllerRef::ChosenPlayer { index }) = tf.controller {
                    return Some(index);
                }
                tf.properties.iter().find_map(|prop| match prop {
                    FilterProp::Owned {
                        controller: ControllerRef::ChosenPlayer { index },
                    } => Some(*index),
                    _ => None,
                })
            }
            TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                filters.iter().find_map(find_index)
            }
            TargetFilter::Not { filter } => find_index(filter),
            _ => None,
        }
    }
    let index = find_index(filter)?;
    ability.chosen_players.get(index as usize).copied()
}

/// True iff the filter constrains matching cards to a specific zone.
fn filter_targets_zone(filter: &TargetFilter, zone: Zone) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::InZone { zone: z } if *z == zone)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(|f| filter_targets_zone(f, zone))
        }
        TargetFilter::Not { filter } => filter_targets_zone(filter, zone),
        _ => false,
    }
}

fn filter_uses_scoped_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(filter) => filter.controller == Some(ControllerRef::ScopedPlayer),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_uses_scoped_player)
        }
        TargetFilter::Not { filter } => filter_uses_scoped_player(filter),
        _ => false,
    }
}

fn stack_spell_casting_variant(
    state: &GameState,
    obj_id: crate::types::identifiers::ObjectId,
) -> Option<CastingVariant> {
    state.stack.iter().find_map(|entry| match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } if entry.id == obj_id => Some(*casting_variant),
        _ => None,
    })
}

/// CR 400.6: Zone change — return target object to the destination zone
/// (default: its owner's hand).
///
/// Also handles LTB self-return triggers (CR 603.10) such as Rancor: when the
/// trigger resolves, the source is already in its owner's graveyard, so the
/// resolver must accept graveyard as a valid from-zone in addition to the
/// battlefield.
///
/// Honors `Effect::Bounce.destination` symmetrically with `BounceAll` below.
/// Today's parser always emits `destination: None` (the canonical "return ...
/// to ... hand" Oracle phrasing); the explicit unwrap default keeps the field
/// meaningful so future parser branches that target other zones (e.g., library
/// top) don't need a separate resolver. CR 608.2c makes the printed destination
/// part of the effect's instructions — silently ignoring a non-null destination
/// would be a rules bug.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 608.2c + 603.10: Delegate target resolution to the unified
    // 3-tier dispatch (`resolved_targets`) so this resolver picks up the
    // same self-ref / event-context / chosen-targets handling that ChangeZone
    // and other zone-change resolvers use. `resolved_targets` short-circuits
    // `SelfRef` to `ability.source_id` regardless of `ability.targets` — this
    // is what makes chained "Exile ~"-style sub-abilities (Treasured Find,
    // Arc Blade, etc.) target the source object rather than inheriting the
    // parent's chosen targets via the chain target-propagation in
    // `effects::mod.rs`.
    let (target_filter, destination, non_targeting) = match &ability.effect {
        Effect::Bounce {
            target,
            destination,
            selection,
        } => (
            target,
            // CR 608.2c: Default to owner's hand — mirrors `BounceAll`'s
            // `destination.unwrap_or(Zone::Hand)` and the canonical Oracle
            // phrasing "return ... to ... hand". Honoring the field makes
            // `Effect::Bounce` symmetric with `Effect::BounceAll` so future
            // parser branches that route through `Bounce` with non-`Hand`
            // destinations don't need a separate resolver.
            destination.unwrap_or(Zone::Hand),
            matches!(selection, BounceSelection::AtResolution),
        ),
        _ => (&TargetFilter::None, Zone::Hand, false),
    };

    let effective_targets = crate::game::targeting::resolved_targets(ability, target_filter, state);
    let targets: Vec<_> = effective_targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .collect();

    // CR 115.1 + Whitemane Lion ruling (issue #563): Non-targeted
    // controller-scoped *battlefield* bounce. Oracle text like "return a
    // creature you control to its owner's hand" parses to a
    // `Bounce { selection: AtResolution, target: Typed{Creature, controller:You}, .. }`.
    // The targeting pipeline does NOT create target slots for non-targeted
    // effects (`extract_target_filter_from_effect` returns `None`), so
    // `targets` is empty here. Enumerate the eligible permanents on the
    // battlefield matching the filter and either fizzle (0), auto-move (1),
    // or surface an `EffectZoneChoice` scoped to the ability controller so
    // they pick which permanent returns (multiple). Mirrors the
    // non-targeted graveyard branch below and the non-targeted
    // `Effect::Sacrifice` resolver path.
    //
    // The graveyard-scope guard below this block excludes graveyard-scoped
    // filters from this branch — they continue to flow into the existing
    // chosen-player graveyard branch which has different selecting-player
    // semantics (Skullwinder-class).
    if non_targeting && targets.is_empty() && !filter_targets_zone(target_filter, Zone::Graveyard) {
        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        let eligible: Vec<_> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                crate::game::filter::matches_target_filter(state, *id, target_filter, &ctx)
            })
            .collect();

        match eligible.len() {
            0 => {
                // CR 608.2d: empty pool — the effect does nothing.
            }
            1 => {
                zones::move_to_zone(state, eligible[0], destination, events);
            }
            _ => {
                // CR 608.2c + CR 608.2d: surface card selection scoped to the
                // ability controller. `EffectKind::ChangeZone` routes through
                // the existing `EffectZoneChoice` intake
                // (`engine_resolution_choices.rs`) which honors `destination`
                // for the battlefield → hand move.
                state.waiting_for = WaitingFor::EffectZoneChoice {
                    player: ability.controller,
                    cards: eligible,
                    count: 1,
                    min_count: 1,
                    up_to: false,
                    source_id: ability.source_id,
                    effect_kind: EffectKind::ChangeZone,
                    zone: Zone::Battlefield,
                    destination: Some(destination),
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_transformed: false,
                    enters_under_player: None,
                    enters_attacking: false,
                    owner_library: false,
                    track_exiled_by_source: false,
                    count_param: 0,
                };
                return Ok(());
            }
        }

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 608.2c + CR 608.2d + CR 109.4 (issue #534): Non-targeted
    // graveyard-return branch. Skullwinder-class effects ("That player
    // returns a card from their graveyard to their hand") parse to a
    // non-targeted `Bounce` whose filter carries
    // `FilterProp::InZone { Graveyard }` and `Owned { ChosenPlayer { index } }`
    // (or `ScopedPlayer` for a pre-`Choose` scope). With no targets and a
    // graveyard-scoped filter, `resolved_targets` returns empty and the
    // standard loop is a no-op — instead, enumerate the selecting player's
    // graveyard against the filter and either move the sole match or surface
    // an `EffectZoneChoice` scoped to that player so they (not the caster)
    // pick which card returns.
    if targets.is_empty() && filter_targets_zone(target_filter, Zone::Graveyard) {
        // The selecting player is the chosen opponent when the filter is
        // chosen-scoped; otherwise fall back to the ability controller
        // (covers same-controller graveyard returns when no `Choose` precedes).
        let selecting_player =
            chosen_player_for_filter(ability, target_filter).unwrap_or(ability.controller);

        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        let matching: Vec<_> = state
            .players
            .iter()
            .find(|p| p.id == selecting_player)
            .map(|p| p.graveyard.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|id| {
                crate::game::filter::matches_target_filter(state, *id, target_filter, &ctx)
            })
            .collect();

        match matching.len() {
            0 => {
                // CR 608.2d: empty pool — the chosen player can't make an
                // impossible choice. Card ruling: "You may choose an opponent
                // with no cards in their graveyard. In that case, they will
                // not get to return anything."
            }
            1 => {
                zones::move_to_zone(state, matching[0], destination, events);
            }
            _ => {
                // CR 608.2d: surface the card selection scoped to the chosen
                // opponent — `player` is the selecting player, NOT the caster.
                // `EffectKind::ChangeZone` routes through the existing
                // `EffectZoneChoice` intake (`engine_resolution_choices.rs`)
                // which honors `destination` for the graveyard → hand move.
                state.waiting_for = WaitingFor::EffectZoneChoice {
                    player: selecting_player,
                    cards: matching,
                    count: 1,
                    min_count: 1,
                    up_to: false,
                    source_id: ability.source_id,
                    effect_kind: EffectKind::ChangeZone,
                    zone: Zone::Graveyard,
                    destination: Some(destination),
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_transformed: false,
                    enters_under_player: None,
                    enters_attacking: false,
                    owner_library: false,
                    track_exiled_by_source: false,
                    count_param: 0,
                };
                return Ok(());
            }
        }

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    for obj_id in targets {
        // CR 114.5: Emblems cannot be bounced
        if state.objects.get(&obj_id).is_some_and(|o| o.is_emblem) {
            continue;
        }

        // CR 400.3 + CR 603.10: Bounce moves the object from its current zone to
        // the destination zone. Battlefield is the usual case; graveyard covers
        // both LTB self-return triggers (Rancor class) and explicit
        // graveyard-targeted return spells (Treasured Find class — `Card` typed
        // filter scoped to graveyard via `InZone` property). CR 112.1 also lets
        // return-to-hand effects move targeted spell objects off the stack;
        // activated and triggered ability stack entries are not cards and are
        // intentionally excluded here.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if matches!(current_zone, Some(Zone::Battlefield | Zone::Graveyard)) {
            zones::move_to_zone(state, obj_id, destination, events);
        } else if current_zone == Some(Zone::Stack) && destination == Zone::Hand {
            if let Some(casting_variant) = stack_spell_casting_variant(state, obj_id) {
                let stack_destination =
                    if casting_variant.exiles_when_leaving_stack_for_any_reason() {
                        Zone::Exile
                    } else {
                        destination
                    };
                zones::move_to_zone(state, obj_id, stack_destination, events);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 400.7 + CR 611.2c: Mass-bounce — return every battlefield permanent
/// matching the filter to its owner's hand (default) or to the destination
/// zone if `Effect::BounceAll.destination` is set.
///
/// Mirrors `destroy::resolve_all` in shape: collect matching object IDs from
/// the battlefield via `crate::game::filter::matches_target_filter`, then
/// move each to the destination zone with `zones::move_to_zone`.
///
/// CR 114.5: Emblems are not on the battlefield (they live in the command
/// zone), so the battlefield scan naturally excludes them — no extra guard
/// needed beyond the existing filter pipeline.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, destination, count_expr) = match &ability.effect {
        Effect::BounceAll {
            target,
            destination,
            count,
        } => (
            target.clone(),
            destination.unwrap_or(Zone::Hand),
            count.as_ref(),
        ),
        _ => (TargetFilter::None, Zone::Hand, None),
    };

    // CR 701.3 + CR 611.2c: A `TargetFilter::None` lands here when the parser
    // didn't supply a class filter. Default to "all creatures" — the
    // historically dominant mass-bounce shape — to match `destroy::resolve_all`.
    let effective_filter = if matches!(target_filter, TargetFilter::None) {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![],
        })
    } else {
        crate::game::effects::resolved_object_filter(ability, &target_filter)
    };
    let scoped_ability;
    let ability = if filter_uses_scoped_player(&effective_filter) && ability.scoped_player.is_none()
    {
        if let Some(player) = ability.targets.iter().find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            TargetRef::Object(_) => None,
        }) {
            scoped_ability = {
                let mut scoped = ability.clone();
                scoped.set_scoped_player_recursive(player);
                scoped
            };
            &scoped_ability
        } else {
            ability
        }
    } else {
        ability
    };

    // CR 107.3a + CR 601.2b: Filter evaluation runs in the ability's
    // resolution context (controller, target slots already filled).
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            crate::game::filter::matches_target_filter(state, **id, &effective_filter, &ctx)
        })
        .copied()
        .collect();

    if let Some(count_expr) = count_expr {
        let count = crate::game::quantity::resolve_quantity_with_targets(state, count_expr, ability)
            .max(0) as usize;
        if count == 0 {
            state.last_effect_count = Some(0);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        if matching.len() > count {
            state.waiting_for = crate::types::game_state::WaitingFor::EffectZoneChoice {
                player: ability.controller,
                cards: matching,
                count,
                min_count: count,
                up_to: false,
                source_id: ability.source_id,
                effect_kind: EffectKind::BounceAll,
                zone: Zone::Battlefield,
                destination: Some(destination),
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_transformed: false,
                enters_under_player: None,
                enters_attacking: false,
                owner_library: false,
                track_exiled_by_source: false,
                count_param: 0,
            };
            return Ok(());
        }
    }

    let mut bounced_ids = Vec::new();
    for &obj_id in &matching {
        // CR 400.3 + CR 400.7: Move each matching permanent to the
        // destination zone. The single-bounce resolver runs the same
        // `zones::move_to_zone` primitive — no replacement-pipeline detour
        // is needed because mass-bounce events are not destruction events
        // (CR 614.6 doesn't apply here).
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if current_zone == Some(Zone::Battlefield) {
            zones::move_to_zone(state, obj_id, destination, events);
            bounced_ids.push(obj_id);
        }
    }

    // CR 603.10a + CR 608.2f: Every permanent in this mass bounce left the
    // battlefield as part of the same resolution event, so leaves-the-battlefield
    // observers among the bounced group observe each other via last-known info.
    zones::mark_simultaneous_departures(events, &zones::departed_subset(state, &bounced_ids));

    state.last_effect_count = Some(matching.len() as i32);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{CastingVariant, StackEntry};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn test_bounce_moves_permanent_to_hand() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_moves_stack_spell_to_hand() {
        let mut state = GameState::new_two_player(42);
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::StackSpell,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.objects.get(&spell_id).unwrap().zone, Zone::Hand);
        assert!(state.players[1].hand.contains(&spell_id));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Stack),
                to: Zone::Hand,
                ..
            } if *object_id == spell_id
        )));
    }

    #[test]
    fn test_bounce_exiles_flashback_stack_spell() {
        let mut state = GameState::new_two_player(42);
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Flashback Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Flashback,
                actual_mana_spent: 0,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::StackSpell,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.objects.get(&spell_id).unwrap().zone, Zone::Exile);
        assert!(state.exile.contains(&spell_id));
        assert!(!state.players[1].hand.contains(&spell_id));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Stack),
                to: Zone::Exile,
                ..
            } if *object_id == spell_id
        )));
    }

    #[test]
    fn test_bounce_does_not_move_stack_ability_to_hand() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Ability Source".to_string(),
            Zone::Battlefield,
        );
        let stack_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Stack Ability".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: stack_id,
            source_id,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    vec![],
                    source_id,
                    PlayerId(1),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Ability Source".to_string(),
                subject_match_count: None,
                die_result: None,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::StackAbility { controller: None },
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(stack_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.stack[0].id, stack_id);
        assert_eq!(state.objects.get(&stack_id).unwrap().zone, Zone::Stack);
        assert!(!state.players[1].hand.contains(&stack_id));
    }

    #[test]
    fn test_bounce_self() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ninja".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::None,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_emits_zone_changed() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    /// CR 608.2c: Single-target `Bounce` honors `destination`, mirroring
    /// `BounceAll`. `Some(Zone::Library)` covers hypothetical "return target
    /// creature to the top of its owner's library" patterns; the resolver
    /// shape is destination-agnostic so future parser branches can route
    /// through it without forking the resolver.
    #[test]
    fn test_bounce_destination_override_routes_to_specified_zone() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: Some(Zone::Library),
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(
            state.players[0].library.contains(&obj_id),
            "destination=Some(Library) must route to the library, not the hand"
        );
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    /// CR 608.2c default: `destination: None` resolves to `Zone::Hand` — the
    /// canonical Oracle phrasing "return ... to ... hand". Building-block
    /// regression: every parser-emitted `Effect::Bounce` carries `None` today,
    /// so this default underpins the entire bounce corpus.
    #[test]
    fn test_bounce_default_destination_is_hand() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].hand.contains(&obj_id));
    }

    /// CR 603.10 / Rancor class: LTB self-return triggers fire after the source
    /// has moved to the graveyard. The parsed effect is
    /// `Bounce { target: ParentTarget }` with empty `ability.targets`; the
    /// resolver must treat that as "return the source object from the graveyard
    /// to its owner's hand."
    #[test]
    fn test_bounce_ltb_self_return_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].graveyard.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_ltb_self_ref_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spirit Loop".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::SelfRef,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![],
            obj_id,
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[1].graveyard.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    /// End-to-end Rancor-class pipeline test: battlefield → graveyard emits
    /// `ZoneChanged`, `process_triggers` picks up the graveyard-zone trigger,
    /// the triggered ability resolves, and the Aura ends up in its owner's hand.
    #[test]
    fn test_rancor_ltb_pipeline_returns_to_owner_hand() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let rancor_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Battlefield,
        );

        // Mirror the shape emitted by the parser for Rancor's LTB trigger.
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
                selection: BounceSelection::Targeted,
            },
        )));
        state
            .objects
            .get_mut(&rancor_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        // Destroy Rancor (move battlefield → graveyard), then run the trigger pipeline.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, rancor_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&rancor_id));

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Rancor LTB trigger did not reach stack"
        );

        // Resolve the triggered ability and confirm Rancor landed in its owner's hand.
        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert!(
            state.players[0].hand.contains(&rancor_id),
            "Rancor should return to owner's hand; actual zones: hand={:?} graveyard={:?}",
            state.players[0].hand,
            state.players[0].graveyard
        );
        assert!(!state.players[0].graveyard.contains(&rancor_id));
    }

    /// CR 400.7 + CR 611.2c: Mass-bounce iterates every battlefield permanent
    /// matching the filter. Mixed match/no-match population: only matching
    /// permanents move to their owners' hands; non-matching permanents stay.
    #[test]
    fn test_bounce_all_creatures_filters_non_creatures() {
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        // Three creatures (P0 owns 2, P1 owns 1) and one artifact owned by P0.
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bear".to_string(),
            Zone::Battlefield,
        );
        let dragon = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Shivan Dragon".to_string(),
            Zone::Battlefield,
        );
        let elf = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        let totem = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Pithing Needle".to_string(),
            Zone::Battlefield,
        );
        // Stamp creature/artifact card_types onto each object so the filter
        // evaluator can classify them.
        for (id, core_types) in [
            (bear, vec![CoreType::Creature]),
            (dragon, vec![CoreType::Creature]),
            (elf, vec![CoreType::Creature]),
            (totem, vec![CoreType::Artifact]),
        ] {
            let obj = state.objects.get_mut(&id).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types,
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let creature_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });
        let ability = ResolvedAbility::new(
            Effect::BounceAll {
                target: creature_filter,
                destination: None,
                count: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All three creatures move to their respective owners' hands.
        assert!(!state.battlefield.contains(&bear), "bear left battlefield");
        assert!(
            !state.battlefield.contains(&dragon),
            "dragon left battlefield"
        );
        assert!(!state.battlefield.contains(&elf), "elf left battlefield");
        assert!(state.players[0].hand.contains(&bear));
        assert!(state.players[0].hand.contains(&dragon));
        assert!(state.players[1].hand.contains(&elf));

        // The artifact stays on the battlefield (filter mismatch).
        assert!(
            state.battlefield.contains(&totem),
            "non-creature totem stays on battlefield"
        );
        assert!(!state.players[0].hand.contains(&totem));
    }

    /// CR 400.7: Destination override threads through `Effect::BounceAll`.
    /// `Some(Zone::Library)` covers hypothetical top-of-library mass-return
    /// patterns (no current corpus card, but the type-system shape mirrors
    /// `Effect::Bounce.destination`).
    #[test]
    fn test_bounce_all_destination_library() {
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let ability = ResolvedAbility::new(
            Effect::BounceAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                destination: Some(Zone::Library),
                count: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&bear));
        assert!(
            state.players[0].library.contains(&bear),
            "bear moved to library when destination override is set"
        );
        assert!(!state.players[0].hand.contains(&bear));
    }

    #[test]
    fn counted_bounce_all_prompts_controller_for_subset() {
        use crate::types::ability::{ControllerRef, QuantityExpr, QuantityRef};

        let mut state = GameState::new_two_player(42);
        let opp_bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Bear".to_string(),
            Zone::Battlefield,
        );
        let opp_dragon = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Dragon".to_string(),
            Zone::Battlefield,
        );
        let own_elf = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Controller Elf".to_string(),
            Zone::Battlefield,
        );

        for id in [opp_bear, opp_dragon, own_elf] {
            let obj = state.objects.get_mut(&id).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let target =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::ScopedPlayer));
        let ability = ResolvedAbility::new(
            Effect::BounceAll {
                target: target.clone(),
                destination: None,
                count: Some(QuantityExpr::DivideRounded {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter: target },
                    }),
                    divisor: 2,
                    rounding: crate::types::ability::RoundingMode::Up,
                }),
            },
            vec![crate::types::ability::TargetRef::Player(PlayerId(1))],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        match state.waiting_for {
            crate::types::game_state::WaitingFor::EffectZoneChoice {
                player,
                count,
                cards,
                effect_kind: EffectKind::BounceAll,
                zone: Zone::Battlefield,
                destination: Some(Zone::Hand),
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert!(cards.contains(&opp_bear));
                assert!(cards.contains(&opp_dragon));
                assert!(!cards.contains(&own_elf));
            }
            ref other => panic!("expected BounceAll EffectZoneChoice, got {other:?}"),
        }
    }

    /// CR 115.1 + Whitemane Lion ruling (issue #563): Non-targeted
    /// controller-scoped bounce with a single eligible permanent auto-moves
    /// without prompting. Mirrors the graveyard branch's single-match path.
    #[test]
    fn test_bounce_non_targeting_controller_scope_single_eligible_auto_moves() {
        use crate::types::ability::TypeFilter;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        // P0 controls one creature; P1 controls a creature too but the filter
        // is `controller: You` so only P0's creature is eligible.
        let own_bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let opp_bear = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Bear".to_string(),
            Zone::Battlefield,
        );
        for id in [own_bear, opp_bear] {
            let obj = state.objects.get_mut(&id).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
                ),
                destination: None,
                selection: BounceSelection::AtResolution,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // P0's creature moved to hand; P1's creature stayed on battlefield.
        assert!(state.players[0].hand.contains(&own_bear));
        assert!(!state.battlefield.contains(&own_bear));
        assert!(state.battlefield.contains(&opp_bear));
        // No prompt — single-eligible auto-move path.
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));
    }

    /// CR 115.1 + Whitemane Lion ruling (issue #563): Non-targeted
    /// controller-scoped bounce with multiple eligible permanents surfaces an
    /// `EffectZoneChoice` so the controller picks. Mirrors the graveyard
    /// branch's multi-match path.
    #[test]
    fn test_bounce_non_targeting_controller_scope_multiple_eligible_prompts_choice() {
        use crate::types::ability::TypeFilter;
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let own_bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let own_dragon = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Dragon".to_string(),
            Zone::Battlefield,
        );
        for id in [own_bear, own_dragon] {
            let obj = state.objects.get_mut(&id).unwrap();
            let card_type = crate::types::card_type::CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            };
            obj.card_types = card_type.clone();
            obj.base_card_types = card_type;
        }

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
                ),
                destination: None,
                selection: BounceSelection::AtResolution,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Both creatures still on battlefield; controller prompted to choose
        // one via EffectZoneChoice.
        assert!(state.battlefield.contains(&own_bear));
        assert!(state.battlefield.contains(&own_dragon));
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                effect_kind: EffectKind::ChangeZone,
                zone: Zone::Battlefield,
                destination: Some(Zone::Hand),
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert_eq!(*min_count, 1);
                assert!(cards.contains(&own_bear));
                assert!(cards.contains(&own_dragon));
            }
            other => panic!("expected EffectZoneChoice for non-targeted bounce, got {other:?}"),
        }
    }

    /// CR 115.1 + Whitemane Lion ruling (issue #563): When no eligible
    /// permanent matches the filter (empty pool), the effect fizzles —
    /// emits `EffectResolved` without crashing or prompting. Models the
    /// Whitemane Lion case where the controller has no other creatures.
    #[test]
    fn test_bounce_non_targeting_controller_scope_empty_eligible_fizzles() {
        use crate::types::ability::TypeFilter;

        let mut state = GameState::new_two_player(42);
        // No creatures on the battlefield — the filter yields an empty pool.

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
                ),
                destination: None,
                selection: BounceSelection::AtResolution,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // No prompt; EffectResolved event emitted.
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }
}

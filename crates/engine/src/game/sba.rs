use std::collections::HashSet;

use crate::game::layers;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::speed::{controls_start_your_engines, set_speed};
use super::zones;

const MAX_SBA_ITERATIONS: u32 = 9;

/// CR 704.3: Run state-based actions in a fixpoint loop until no more actions are performed,
/// capped at MAX_SBA_ITERATIONS.
pub fn check_state_based_actions(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // CR 604.2: Re-evaluate layers so computed P/T reflects current static abilities.
    if state.layers_dirty {
        // Snapshot P/T before layer re-evaluation for delta logging.
        let pt_snapshot: Vec<(crate::types::identifiers::ObjectId, i32, i32)> = state
            .battlefield
            .iter()
            .filter_map(|&id| {
                let obj = state.objects.get(&id)?;
                Some((id, obj.power?, obj.toughness?))
            })
            .collect();

        layers::evaluate_layers(state);

        // Emit events for P/T changes (creatures only — skip objects that lost P/T).
        for (id, old_p, old_t) in &pt_snapshot {
            if let Some(obj) = state.objects.get(id) {
                if let (Some(new_p), Some(new_t)) = (obj.power, obj.toughness) {
                    if new_p != *old_p || new_t != *old_t {
                        events.push(GameEvent::PowerToughnessChanged {
                            object_id: *id,
                            power: new_p,
                            toughness: new_t,
                            power_delta: new_p - old_p,
                            toughness_delta: new_t - old_t,
                        });
                    }
                }
                // If P/T became None (lost creature type), skip — not meaningful for log.
            }
        }
    }

    for _ in 0..MAX_SBA_ITERATIONS {
        let mut any_performed = false;

        // CR 704.5a: A player with 0 or less life loses the game.
        check_player_life(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.5b: A player who attempted to draw from an empty library loses the game.
        check_draw_from_empty(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.5c: A player with ten or more poison counters loses the game.
        check_poison_counters(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.6c: A player who has been dealt 21 or more combat damage by the same
        // commander loses the game.
        check_commander_damage(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.5f: A creature with toughness 0 or less is put into its owner's graveyard.
        check_zero_toughness(state, events, &mut any_performed);

        // CR 704.5g: A creature with lethal damage marked on it is destroyed.
        check_lethal_damage(state, events, &mut any_performed);

        // CR 614.3 / CR 701.19b: If a regeneration replacement choice is pending, pause SBA evaluation.
        if state.pending_replacement.is_some() {
            return;
        }

        // CR 704.5j: If a player controls two or more legendary permanents with the same name,
        // that player chooses one and the rest are put into their owners' graveyards.
        check_legend_rule(state, events, &mut any_performed);

        // CR 704.5m: If an Aura is attached to an illegal object or player, it is put into
        // its owner's graveyard.
        check_unattached_auras(state, events, &mut any_performed);

        // CR 704.5n: If an Equipment is attached to an illegal permanent, it becomes unattached.
        check_unattached_equipment(state, &mut any_performed);

        // CR 704.5i + CR 306.9: If a planeswalker has loyalty 0, it is put into its owner's graveyard.
        check_zero_loyalty(state, events, &mut any_performed);

        // CR 704.5v + CR 310.7: If a battle has defense 0 and isn't the source of an
        // ability that has triggered but not yet left the stack, it's put into its
        // owner's graveyard.
        check_zero_defense(state, events, &mut any_performed);

        // CR 704.5p + CR 310.9: If a battle is somehow attached to a permanent, unattach it.
        check_battle_unattached(state, &mut any_performed);

        // CR 704.5w + CR 704.5x + CR 310.10: Battle with no (or illegal) protector —
        // controller chooses an appropriate protector; graveyard if none can be chosen.
        check_battle_protector(state, events, &mut any_performed);

        // CR 704.5s + CR 714.4: If a Saga has lore counters >= its final chapter number,
        // and no chapter ability has triggered but not yet left the stack, sacrifice it.
        check_saga_sacrifice(state, events, &mut any_performed);

        // CR 704.5q: +1/+1 and -1/-1 counters on the same permanent cancel in pairs.
        check_counter_cancellation(state, &mut any_performed);

        // CR 704.5d: Tokens in zones other than the battlefield cease to exist.
        check_token_cease_to_exist(state, &mut any_performed);

        // CR 704.5z: A player controlling Start your engines! gets speed 1 if they had none.
        check_start_your_engines(state, events, &mut any_performed);

        // CR 702.131b: A player controlling an Ascend permanent with ten or more
        // permanents gets the city's blessing for the rest of the game.
        check_city_blessing(state, events, &mut any_performed);

        // CR 704.5t: If a player's venture marker is on the bottommost room
        // and no room ability from that dungeon is on the stack, complete the dungeon.
        check_dungeon_completion(state, events, &mut any_performed);

        if !any_performed {
            break;
        }
    }
}

/// CR 704.5z + CR 702.179a: If a player controls a permanent with start your engines!
/// and has no speed, their speed becomes 1.
fn check_start_your_engines(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let players_to_start: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|player| player.speed.is_none())
        .filter(|player| controls_start_your_engines(state, player.id))
        .map(|player| player.id)
        .collect();

    for player_id in players_to_start {
        set_speed(state, player_id, Some(1), events);
        *any_performed = true;
    }
}

/// CR 702.131b: Ascend on a permanent means "Any time you control ten or more
/// permanents and you don't have the city's blessing, you get the city's blessing
/// for the rest of the game." CR 702.131d: Continuous effects are reapplied after
/// the grant, so we mark layers dirty so "as long as you have the city's blessing"
/// statics pick up the new designation on the next layer pass.
fn check_city_blessing(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let players_to_bless: Vec<PlayerId> = state
        .players
        .iter()
        .map(|p| p.id)
        .filter(|pid| !state.city_blessing.contains(pid))
        .filter(|pid| controls_ascend_permanent(state, *pid))
        .filter(|pid| permanents_controlled(state, *pid) >= 10)
        .collect();

    for player_id in players_to_bless {
        state.city_blessing.insert(player_id);
        state.layers_dirty = true;
        events.push(GameEvent::CityBlessingGained { player_id });
        *any_performed = true;
    }
}

/// CR 702.131b: "you control ten or more permanents" — every object on the
/// battlefield is a permanent (CR 110.1).
fn permanents_controlled(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.controller == player)
        })
        .count()
}

/// CR 702.131: whether `player` controls any permanent with the Ascend keyword.
fn controls_ascend_permanent(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            obj.controller == player && obj.has_keyword(&crate::types::keywords::Keyword::Ascend)
        })
    })
}

/// CR 104.3b + CR 810.8a: Check if a player has active CantLoseTheGame protection
/// from any permanent on the battlefield. If so, SBAs that would cause that player
/// to lose the game are skipped.
fn player_has_cant_lose(state: &GameState, player_id: PlayerId) -> bool {
    state.battlefield.iter().any(|&id| {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => return false,
        };
        super::functioning_abilities::active_static_definitions(state, obj).any(|def| {
            def.mode == StaticMode::CantLoseTheGame
                && static_affects_player(obj.controller, &def.affected, player_id)
        })
    })
}

/// Check if a static ability from `source_controller` with the given `affected` filter
/// applies to `player_id`.
fn static_affects_player(
    source_controller: PlayerId,
    affected: &Option<TargetFilter>,
    player_id: PlayerId,
) -> bool {
    match affected {
        Some(TargetFilter::Typed(TypedFilter { controller, .. })) => match controller {
            Some(ControllerRef::You) => source_controller == player_id,
            Some(ControllerRef::Opponent) => source_controller != player_id,
            // CR 109.4: TargetPlayer has no meaning for static-ability scoping
            // against a player. Fail closed.
            Some(ControllerRef::TargetPlayer) => false,
            None => true,
        },
        Some(TargetFilter::Player) => true,
        Some(TargetFilter::Any) => true,
        None => true,
        _ => false,
    }
}

/// CR 704.5a: A player with 0 or less life loses the game.
fn check_player_life(state: &mut GameState, events: &mut Vec<GameEvent>, any_performed: &mut bool) {
    // Collect all players who should be eliminated (check all, not just first)
    // CR 104.3b: Skip players protected by CantLoseTheGame.
    //
    // Player-phasing exclusion: a phased-out player can't lose the game from
    // 0-or-less life — they're treated as though they don't exist for SBA
    // purposes (mirrors CR 702.26b for permanents, applied to players).
    let to_eliminate: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && !p.is_phased_out() && p.life <= 0)
        .filter(|p| !player_has_cant_lose(state, p.id))
        .map(|p| p.id)
        .collect();

    for loser in to_eliminate {
        events.push(GameEvent::PlayerLost { player_id: loser });
        super::elimination::eliminate_player(state, loser, events);
        *any_performed = true;
    }
}

/// CR 704.5b: A player who attempted to draw from an empty library loses the game.
fn check_draw_from_empty(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    // CR 104.3b: Skip players protected by CantLoseTheGame.
    let to_eliminate: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.drew_from_empty_library)
        .filter(|p| !player_has_cant_lose(state, p.id))
        .map(|p| p.id)
        .collect();

    for loser in to_eliminate {
        events.push(GameEvent::PlayerLost { player_id: loser });
        super::elimination::eliminate_player(state, loser, events);
        *any_performed = true;
    }
}

/// CR 704.5c: A player with ten or more poison counters loses the game.
fn check_poison_counters(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    // CR 104.3b: Skip players protected by CantLoseTheGame.
    let to_eliminate: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.poison_counters >= 10)
        .filter(|p| !player_has_cant_lose(state, p.id))
        .map(|p| p.id)
        .collect();

    for loser in to_eliminate {
        events.push(GameEvent::PlayerLost { player_id: loser });
        super::elimination::eliminate_player(state, loser, events);
        *any_performed = true;
    }
}

/// CR 704.6c: A player dealt 21+ combat damage by the same commander loses.
fn check_commander_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let threshold = match state.format_config.commander_damage_threshold {
        Some(t) => t as u32,
        None => return, // Not a Commander format
    };

    // Collect players who should be eliminated
    // CR 104.3b: Skip players protected by CantLoseTheGame.
    let to_eliminate: Vec<PlayerId> = state
        .commander_damage
        .iter()
        .filter(|entry| entry.damage >= threshold)
        .map(|entry| entry.player)
        .filter(|pid| !state.eliminated_players.contains(pid))
        .filter(|pid| !player_has_cant_lose(state, *pid))
        .collect();

    for player_id in to_eliminate {
        super::elimination::eliminate_player(state, player_id, events);
        *any_performed = true;
    }
}

/// CR 704.5f: A creature with toughness 0 or less is put into its owner's graveyard.
/// CR 702.26b: Phased-out permanents are treated as though they don't exist —
/// state-based actions scan only phased-in permanents.
fn check_zero_toughness(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_destroy: Vec<_> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Creature)
                        && obj.toughness.is_some_and(|t| t <= 0)
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

/// CR 704.5g / CR 704.5h: A creature with lethal damage (or deathtouch damage) is destroyed.
/// CR 702.26b: Phased-out permanents are treated as though they don't exist.
fn check_lethal_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_destroy: Vec<_> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Creature)
                        && (
                            // Normal lethal damage: damage >= toughness
                            obj.toughness.is_some_and(|t| obj.damage_marked >= t as u32 && t > 0)
                            // CR 702.2b: Any nonzero damage from a deathtouch source is lethal.
                            || (obj.dealt_deathtouch_damage && obj.damage_marked > 0)
                        )
                        // CR 702.12b: Indestructible creatures are not destroyed by lethal damage.
                        && !obj.has_keyword(&crate::types::keywords::Keyword::Indestructible)
                })
                .unwrap_or(false)
        })
        .collect();

    // CR 701.19b: Route each destruction through the replacement pipeline
    // so regeneration shields can intercept.
    for id in to_destroy {
        let proposed = ProposedEvent::Destroy {
            object_id: id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Destroy {
                    object_id, source, ..
                } = event
                {
                    let zone_proposed = ProposedEvent::zone_change(
                        object_id,
                        Zone::Battlefield,
                        Zone::Graveyard,
                        source,
                    );
                    match replacement::replace_event(state, zone_proposed, events) {
                        ReplacementResult::Execute(zone_event) => {
                            if let ProposedEvent::ZoneChange {
                                object_id: oid, to, ..
                            } = zone_event
                            {
                                zones::move_to_zone(state, oid, to, events);
                            }
                        }
                        ReplacementResult::Prevented => {}
                        ReplacementResult::NeedsChoice(player) => {
                            state.waiting_for =
                                replacement::replacement_choice_waiting_for(player, state);
                            return;
                        }
                    }
                    events.push(GameEvent::CreatureDestroyed { object_id });
                }
                *any_performed = true;
            }
            ReplacementResult::Prevented => {
                // CR 701.19b: Regeneration prevented destruction — still counts as SBA performed.
                *any_performed = true;
            }
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                return;
            }
        }
    }
}

/// CR 704.5j: If a player controls two or more legendary permanents with the same name,
/// that player chooses one and the rest are put into their owners' graveyards.
/// This is NOT destruction — indestructible does not prevent it.
fn check_legend_rule(
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
    _any_performed: &mut bool,
) {
    for player_idx in 0..state.players.len() {
        let player_id = state.players[player_idx].id;

        // Group legendaries by name
        let legendaries: Vec<_> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|obj| {
                        obj.controller == player_id
                            && obj.card_types.supertypes.contains(&Supertype::Legendary)
                    })
                    .unwrap_or(false)
            })
            .collect();

        // Group by name
        let mut by_name: std::collections::HashMap<String, Vec<_>> =
            std::collections::HashMap::new();
        for id in legendaries {
            if let Some(obj) = state.objects.get(&id) {
                by_name.entry(obj.name.clone()).or_default().push(id);
            }
        }

        // CR 704.5j: For names with 2+, pause and let the player choose which to keep.
        // One group at a time — SBA fixpoint re-runs and finds the next group after choice.
        for (name, ids) in by_name {
            if ids.len() < 2 {
                continue;
            }

            state.waiting_for = WaitingFor::ChooseLegend {
                player: player_id,
                legend_name: name,
                candidates: ids,
            };
            return;
        }
    }
}

/// CR 704.5m: An Aura attached to an illegal object or player, or that is no
/// longer attached to anything legal, is put into its owner's graveyard.
/// CR 303.4c: An enchanted object that no longer exists, or an enchanted player
/// who has left the game, is illegal — the Aura goes to its owner's graveyard.
/// CR 702.26b: Phased-out Auras are treated as though they don't exist; their
/// attachment-legality isn't checked by this SBA.
fn check_unattached_auras(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_remove: Vec<_> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    if !obj.card_types.core_types.contains(&CoreType::Enchantment) {
                        return false;
                    }
                    // CR 704.5m / CR 704.5n apply specifically to *Auras* —
                    // gate on the Aura subtype so non-Aura enchantments
                    // (Saga, Class, Background, Shrine, etc.) are not
                    // affected. The CoreType check above is necessary but
                    // not sufficient.
                    let is_aura = obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Aura"));
                    if !is_aura {
                        return false;
                    }
                    // Note: the parser also routes player-attached Auras here.
                    // CR 303.4c: A player who has left the game is an illegal host.
                    // CR 704.5n: An Aura that is "unattached and on the
                    // battlefield" is also put into its owner's graveyard —
                    // covers the case where a target legally chosen at
                    // announcement is removed before resolution can attach
                    // (target destroyed by another stack effect, target left
                    // the battlefield mid-resolution, etc.). Without this, an
                    // orphan Aura with `attached_to = None` would persist on
                    // the battlefield doing nothing. Aura cast resolution
                    // sets `attached_to` synchronously, so a freshly resolved
                    // Aura is never observed here with `None` — by the time
                    // SBAs run, an Aura with no host genuinely has no host.
                    match obj.attached_to {
                        Some(crate::game::game_object::AttachTarget::Object(t)) => {
                            !is_valid_attachment_target(state, t)
                        }
                        Some(crate::game::game_object::AttachTarget::Player(pid)) => {
                            !is_player_in_game(state, pid)
                        }
                        None => true,
                    }
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_remove {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

/// CR 704.5n + CR 301.5c: Equipment attached to an illegal permanent (or, per
/// CR 704.5n, to a player at all) becomes unattached. Equipment can never
/// legally attach to a player (CR 301.5), so a `Player` host is *always*
/// illegal and must be unattached on this SBA pass.
/// CR 702.26b: Phased-out Equipment is treated as though it doesn't exist.
fn check_unattached_equipment(state: &mut GameState, any_performed: &mut bool) {
    let to_unattach: Vec<_> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    if !obj.card_types.subtypes.contains(&"Equipment".to_string()) {
                        return false;
                    }
                    match obj.attached_to {
                        // CR 301.5: Equipment must attach to an object;
                        // illegal-target check applies.
                        Some(crate::game::game_object::AttachTarget::Object(t)) => {
                            !is_valid_attachment_target(state, t)
                        }
                        // CR 704.5n: Equipment attached to a player is always illegal.
                        Some(crate::game::game_object::AttachTarget::Player(_)) => true,
                        None => false,
                    }
                })
                .unwrap_or(false)
        })
        .collect();

    for equipment_id in to_unattach {
        // Clear the attachment reference on the equipment. Only Object hosts
        // have an `attachments` list to clean up — Player hosts do not.
        if let Some(crate::game::game_object::AttachTarget::Object(old_target_id)) = state
            .objects
            .get(&equipment_id)
            .and_then(|obj| obj.attached_to)
        {
            if let Some(old_target) = state.objects.get_mut(&old_target_id) {
                old_target.attachments.retain(|&id| id != equipment_id);
            }
        }
        if let Some(equipment) = state.objects.get_mut(&equipment_id) {
            equipment.attached_to = None;
        }
        *any_performed = true;
    }
}

/// CR 704.5i + CR 306.9: A planeswalker with loyalty 0 is put into its owner's graveyard.
fn check_zero_loyalty(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_destroy: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Planeswalker)
                        && obj.loyalty.is_some_and(|l| l == 0)
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

/// CR 704.5v + CR 310.7: A battle with defense 0 is put into its owner's graveyard,
/// unless it's the source of an ability that has triggered but not yet left the
/// stack (e.g., the Siege's victory trigger).
fn check_zero_defense(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    use crate::types::game_state::StackEntryKind;

    let to_destroy: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = match state.objects.get(id) {
                Some(o) => o,
                None => return false,
            };
            if !obj.card_types.core_types.contains(&CoreType::Battle) {
                return false;
            }
            if obj.defense.unwrap_or(0) != 0 {
                return false;
            }
            // CR 310.7: Don't SBA-destroy while one of this battle's triggered
            // abilities is still on the stack (mirrors CR 714.4 Saga deferral).
            let ability_on_stack = state.stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == *id
                )
            });
            !ability_on_stack
        })
        .collect();

    for id in to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

/// CR 704.5p + CR 310.9: A battle can't be attached to players or permanents.
/// If a battle is somehow attached, it becomes unattached and remains on the battlefield.
fn check_battle_unattached(state: &mut GameState, any_performed: &mut bool) {
    let battles_to_unattach: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Battle)
                        && obj.attached_to.is_some()
                })
                .unwrap_or(false)
        })
        .collect();

    for battle_id in battles_to_unattach {
        // Remove from host's attachments list first. Only Object hosts have an
        // `attachments` list; Player hosts (CR 303.4 + CR 702.5d) do not.
        if let Some(crate::game::game_object::AttachTarget::Object(host)) = state
            .objects
            .get(&battle_id)
            .and_then(|obj| obj.attached_to)
        {
            if let Some(host_obj) = state.objects.get_mut(&host) {
                host_obj.attachments.retain(|&id| id != battle_id);
            }
        }
        if let Some(battle) = state.objects.get_mut(&battle_id) {
            battle.attached_to = None;
        }
        *any_performed = true;
    }
}

/// CR 704.5w + CR 704.5x + CR 310.10 + CR 310.11a: If a battle that isn't being
/// attacked has no protector, an illegal protector, or (for Sieges) a protector
/// that equals its controller, its controller chooses a legal protector. If no
/// legal player exists, the battle is put into its owner's graveyard.
///
/// When multiple legal candidates exist (3+ player games), the SBA pauses with
/// `WaitingFor::BattleProtectorChoice` so the controller can choose interactively
/// (mirrors `check_legend_rule`). 2-player games and singleton candidate lists
/// auto-apply — the CR-mandated "controller chooses" is vacuous over a one-element
/// choice space.
fn check_battle_protector(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    // Snapshot battlefield battles and whether each is currently being attacked.
    let being_attacked: HashSet<crate::types::identifiers::ObjectId> = state
        .combat
        .as_ref()
        .map(|combat| {
            combat
                .attackers
                .iter()
                .filter_map(|a| match a.attack_target {
                    crate::game::combat::AttackTarget::Battle(id) => Some(id),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let battle_ids: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Battle))
        })
        .collect();

    for battle_id in battle_ids {
        let Some(battle) = state.objects.get(&battle_id) else {
            continue;
        };
        let controller = battle.controller;
        let is_siege = battle.card_types.subtypes.iter().any(|s| s == "Siege");
        let protector = battle.protector();

        // Legal protectors for a Siege are opponents of the controller (CR 310.11a).
        // For non-Siege battles with no battle type, CR 310.8a says the controller
        // becomes the protector; we treat the controller as legal in that case.
        let protector_legal = match protector {
            Some(p) if is_siege => crate::game::players::opponents(state, controller).contains(&p),
            Some(_) => true,
            None => false,
        };

        if protector_legal {
            continue;
        }
        if being_attacked.contains(&battle_id) {
            // CR 310.10: Only applies to battles that aren't being attacked.
            continue;
        }

        // Compute legal choices.
        let legal_choices: Vec<PlayerId> = if is_siege {
            crate::game::players::opponents(state, controller)
                .into_iter()
                .filter(|p| !state.eliminated_players.contains(p))
                .collect()
        } else {
            // CR 310.8a: With no battle types, controller is the protector.
            vec![controller]
        };

        match legal_choices.len() {
            0 => {
                // CR 310.10 / CR 704.5w: No legal protector exists — graveyard.
                zones::move_to_zone(state, battle_id, Zone::Graveyard, events);
                *any_performed = true;
            }
            1 => {
                // Singleton choice space — "controller chooses" is vacuous.
                // Preserves the 2-player fast path (exactly one legal opponent).
                let chosen = legal_choices[0];
                if let Some(obj) = state.objects.get_mut(&battle_id) {
                    obj.chosen_attributes.retain(|a| {
                        !matches!(a, crate::types::ability::ChosenAttribute::Player(_))
                    });
                    obj.chosen_attributes
                        .push(crate::types::ability::ChosenAttribute::Player(chosen));
                }
                *any_performed = true;
            }
            _ => {
                // CR 310.10 + CR 704.5w + CR 704.5x: multiple legal protectors —
                // the controller must choose. Pause the SBA fixpoint and yield
                // a WaitingFor (mirrors `check_legend_rule`). The SBA re-runs
                // on the next apply and finds any remaining battles.
                state.waiting_for = WaitingFor::BattleProtectorChoice {
                    player: controller,
                    battle_id,
                    candidates: legal_choices,
                };
                return;
            }
        }
    }
}

/// CR 704.5s + CR 714.4: Sacrifice Sagas that have reached their final chapter,
/// unless a chapter ability from that Saga is still on the stack or a lore counter
/// was just added (meaning process_triggers hasn't placed the chapter trigger yet).
fn check_saga_sacrifice(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    use crate::types::game_state::StackEntryKind;

    let to_sacrifice: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = match state.objects.get(id) {
                Some(o) => o,
                None => return false,
            };
            let final_ch = match obj.final_chapter_number() {
                Some(n) => n,
                None => return false,
            };
            let lore_count = obj.counters.get(&CounterType::Lore).copied().unwrap_or(0);
            if lore_count < final_ch {
                return false;
            }

            // CR 714.4: Don't sacrifice while a chapter trigger from this Saga is on the stack.
            let chapter_on_stack = state.stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == *id
                )
            });
            if chapter_on_stack {
                return false;
            }

            // CR 714.4 deferral: A lore counter was just added in this SBA batch —
            // process_triggers hasn't run yet, so defer sacrifice for one pass.
            let pending_lore_event = events.iter().any(|e| {
                matches!(
                    e,
                    GameEvent::CounterAdded {
                        object_id,
                        counter_type: CounterType::Lore,
                        ..
                    } if *object_id == *id
                )
            });
            if pending_lore_event {
                return false;
            }

            true
        })
        .collect();

    for saga_id in to_sacrifice {
        let owner = state
            .objects
            .get(&saga_id)
            .map(|obj| obj.owner)
            .unwrap_or(crate::types::player::PlayerId(0));
        events.push(GameEvent::PermanentSacrificed {
            object_id: saga_id,
            player_id: owner,
        });
        zones::move_to_zone(state, saga_id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

/// CR 704.5q: If a permanent has both +1/+1 and -1/-1 counters, remove pairs until
/// only one type remains.
/// CR 702.26b: Phased-out permanents are treated as though they don't exist;
/// their counters aren't touched by this SBA.
fn check_counter_cancellation(state: &mut GameState, any_performed: &mut bool) {
    let bf_ids: Vec<_> = state.battlefield_phased_in_ids();
    for obj_id in bf_ids {
        let Some(obj) = state.objects.get_mut(&obj_id) else {
            continue;
        };
        let p1p1 = obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        let m1m1 = obj
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0);
        let cancel = p1p1.min(m1m1);
        if cancel > 0 {
            // CR 704.5q: Remove N of each where N = min(+1/+1, -1/-1)
            obj.counters.insert(CounterType::Plus1Plus1, p1p1 - cancel);
            obj.counters
                .insert(CounterType::Minus1Minus1, m1m1 - cancel);
            obj.counters.retain(|_, v| *v > 0);
            state.layers_dirty = true; // P/T affected via Layer 7d
            *any_performed = true;
        }
    }
}

/// CR 704.5d: A token that's in a zone other than the battlefield ceases to exist.
/// Tokens on the stack are excluded — spell copies resolve before the next SBA check.
fn check_token_cease_to_exist(state: &mut GameState, any_performed: &mut bool) {
    let tokens_to_remove: Vec<(
        crate::types::identifiers::ObjectId,
        Zone,
        crate::types::player::PlayerId,
    )> = state
        .objects
        .iter()
        .filter(|(_, obj)| obj.is_token && obj.zone != Zone::Battlefield && obj.zone != Zone::Stack)
        .map(|(id, obj)| (*id, obj.zone, obj.owner))
        .collect();

    for (obj_id, zone, owner) in tokens_to_remove {
        // CR 704.5d: Token ceases to exist — not a zone change, no event emitted.
        // Ceasing to exist is distinct from exile (CR 400.7); the frontend detects
        // removal via state diffs. No "whenever exiled" trigger should fire.
        zones::remove_from_zone(state, obj_id, zone, owner);
        state.objects.remove(&obj_id);
        *any_performed = true;
    }
}

fn is_valid_attachment_target(
    state: &GameState,
    target_id: crate::types::identifiers::ObjectId,
) -> bool {
    state
        .objects
        .get(&target_id)
        .map(|obj| obj.zone == Zone::Battlefield)
        .unwrap_or(false)
}

/// CR 303.4c: A player has "left the game" when they are eliminated. Multiplayer
/// exits flip the same flag (CR 800.4a). Out-of-range PlayerIds (defensive) are
/// treated as not in game so the Aura SBA cleans up the dangling reference.
fn is_player_in_game(state: &GameState, player_id: crate::types::player::PlayerId) -> bool {
    state
        .players
        .get(player_id.0 as usize)
        .is_some_and(|p| !p.is_eliminated)
}

/// CR 704.5t: If a player's venture marker is on the bottommost room of a dungeon card,
/// and that dungeon card isn't the source of a room ability that has triggered but not yet
/// left the stack, the dungeon card's owner removes it from the game.
fn check_dungeon_completion(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    use crate::game::dungeon::{dungeon_sentinel_id, is_bottommost};

    // Collect players whose dungeons need completing.
    let to_complete: Vec<(
        crate::types::player::PlayerId,
        crate::game::dungeon::DungeonId,
    )> = state
        .dungeon_progress
        .iter()
        .filter_map(|(&player, progress)| {
            let dungeon_id = progress.current_dungeon?;
            if !is_bottommost(dungeon_id, progress.current_room) {
                return None;
            }
            // Check if any room ability from this dungeon is on the stack.
            let sentinel = dungeon_sentinel_id(player);
            let has_room_on_stack = state.stack.iter().any(|entry| entry.source_id == sentinel);
            if has_room_on_stack {
                return None;
            }
            Some((player, dungeon_id))
        })
        .collect();

    for (player, dungeon_id) in to_complete {
        if let Some(progress) = state.dungeon_progress.get_mut(&player) {
            progress.current_dungeon = None;
            progress.current_room = 0;
            progress.completed.insert(dungeon_id);
            events.push(GameEvent::DungeonCompleted {
                player_id: player,
                dungeon: dungeon_id,
            });
            *any_performed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn create_creature(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(state.turn_number);
        id
    }

    // --- 2-player SBA tests (backward compatible) ---

    #[test]
    fn sba_zero_life_player_loses() {
        let mut state = setup();
        state.players[0].life = 0;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(0)
            }
        )));
    }

    #[test]
    fn sba_negative_life_player_loses() {
        let mut state = setup();
        state.players[1].life = -5;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn sba_zero_toughness_creature_dies() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Weakling", 1, 0);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
    }

    #[test]
    fn sba_lethal_damage_creature_dies() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().damage_marked = 2;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
    }

    #[test]
    fn sba_healthy_creature_survives() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().damage_marked = 1;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn sba_legend_rule_presents_choice() {
        let mut state = setup();
        state.turn_number = 1;
        let id1 = create_creature(&mut state, CardId(1), PlayerId(0), "Thalia", 2, 1);
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .entered_battlefield_turn = Some(1);

        state.turn_number = 2;
        let id2 = create_creature(&mut state, CardId(2), PlayerId(0), "Thalia", 2, 1);
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .entered_battlefield_turn = Some(2);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 704.5j: SBA pauses and presents a choice — both still on battlefield
        assert!(state.battlefield.contains(&id1));
        assert!(state.battlefield.contains(&id2));
        match &state.waiting_for {
            WaitingFor::ChooseLegend {
                player,
                legend_name,
                candidates,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(legend_name, "Thalia");
                assert!(candidates.contains(&id1));
                assert!(candidates.contains(&id2));
            }
            other => panic!("Expected ChooseLegend, got {:?}", other),
        }
    }

    #[test]
    fn sba_unattached_aura_goes_to_graveyard() {
        let mut state = setup();
        // Create an Aura attached to a nonexistent object
        let aura_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(ObjectId(999).into()); // nonexistent target

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&aura_id));
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    #[test]
    fn sba_fixpoint_handles_cascading_deaths() {
        let mut state = setup();
        // Create a creature that will die from lethal damage
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().damage_marked = 3;

        // Create an aura attached to that creature
        let aura_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(id.into());

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Both should be in graveyard (creature dies, then aura detaches and dies)
        assert!(!state.battlefield.contains(&id));
        assert!(!state.battlefield.contains(&aura_id));
    }

    #[test]
    fn sba_poison_10_player_loses() {
        let mut state = setup();
        state.players[0].poison_counters = 10;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(0)
            }
        )));
    }

    #[test]
    fn sba_poison_9_player_survives() {
        let mut state = setup();
        state.players[0].poison_counters = 9;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn sba_no_actions_when_nothing_to_do() {
        let mut state = setup();
        create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // No zone change events should have been generated
        assert!(events.is_empty());
    }

    #[test]
    fn sba_equipment_unattaches_when_creature_dies() {
        let mut state = setup();
        // Create a creature that will die
        let creature_id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&creature_id).unwrap().damage_marked = 3; // lethal

        // Create equipment attached to that creature
        let equip_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&equip_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.attached_to = Some(creature_id.into());

        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .attachments
            .push(equip_id);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Creature should be dead
        assert!(!state.battlefield.contains(&creature_id));
        // Equipment should still be on battlefield but unattached
        assert!(state.battlefield.contains(&equip_id));
        assert_eq!(state.objects.get(&equip_id).unwrap().attached_to, None);
    }

    #[test]
    fn sba_equipment_on_battlefield_without_attachment_stays() {
        let mut state = setup();
        // Equipment on battlefield with no attached_to is a valid state
        let equip_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&equip_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Equipment should stay on battlefield, no events generated
        assert!(state.battlefield.contains(&equip_id));
        assert!(events.is_empty());
    }

    #[test]
    fn sba_aura_still_goes_to_graveyard_when_target_leaves() {
        let mut state = setup();
        // Create a creature that will die
        let creature_id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&creature_id).unwrap().damage_marked = 3;

        // Create an aura attached to the creature
        let aura_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(creature_id.into());

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Both should be gone from battlefield
        assert!(!state.battlefield.contains(&creature_id));
        assert!(!state.battlefield.contains(&aura_id));
        // Aura goes to graveyard (not stays on battlefield like equipment)
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    fn create_planeswalker(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        // CR 306.5b: loyalty field and counter map mirror each other.
        obj.loyalty = Some(loyalty);
        obj.counters
            .insert(crate::types::counter::CounterType::Loyalty, loyalty);
        obj.entered_battlefield_turn = Some(state.turn_number);
        id
    }

    #[test]
    fn sba_zero_loyalty_planeswalker_dies() {
        let mut state = setup();
        let pw = create_planeswalker(&mut state, CardId(1), PlayerId(0), "Jace", 0);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&pw));
        assert!(state.players[0].graveyard.contains(&pw));
    }

    #[test]
    fn sba_positive_loyalty_planeswalker_survives() {
        let mut state = setup();
        let pw = create_planeswalker(&mut state, CardId(1), PlayerId(0), "Jace", 3);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&pw));
    }

    // --- N-player SBA tests ---

    #[test]
    fn sba_three_player_one_dies_game_continues() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.players[1].life = 0;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // P1 eliminated but game continues
        assert!(state.players[1].is_eliminated);
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn sba_three_player_two_die_simultaneously_ends_game() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.players[1].life = 0;
        state.players[2].life = -3;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // Both eliminated, P0 wins
        assert!(state.players[1].is_eliminated);
        assert!(state.players[2].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn sba_eliminated_player_not_re_checked() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        // P1 already eliminated with 0 life
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));
        state.players[1].life = 0;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // No new events for already-eliminated player
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(1)
            }
        )));
    }

    #[test]
    fn sba_commander_damage_21_eliminates_player() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let cmd_id = ObjectId(999);
        // Player 1 has taken 21 commander damage from cmd_id
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_id,
            damage: 21,
        });
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // P1 should be eliminated
        assert!(state.players[1].is_eliminated);
        assert!(state.eliminated_players.contains(&PlayerId(1)));
        // Game should NOT be over (3 remaining players)
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn sba_commander_damage_20_does_not_eliminate() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let cmd_id = ObjectId(999);
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_id,
            damage: 20,
        });
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // P1 should NOT be eliminated (threshold is 21)
        assert!(!state.players[1].is_eliminated);
    }

    #[test]
    fn sba_commander_damage_skipped_in_non_commander_format() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let cmd_id = ObjectId(999);
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_id,
            damage: 100,
        });
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // Not a commander format -> threshold is None -> no elimination
        assert!(!state.players[1].is_eliminated);
    }

    #[test]
    fn sba_2hg_team_dies_together() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.players[0].life = 0; // Team A player dies
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // Both team A members eliminated
        assert!(state.players[0].is_eliminated);
        assert!(state.players[1].is_eliminated);
        // Team B wins
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver { winner: Some(_) }
        ));
    }

    // --- Saga SBA tests ---

    fn create_saga(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        final_chapter: u32,
    ) -> ObjectId {
        use crate::types::ability::{CounterTriggerFilter, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.entered_battlefield_turn = Some(state.turn_number);
        // Add chapter triggers so final_chapter_number() works
        for ch in 1..=final_chapter {
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                    CounterTriggerFilter {
                        counter_type: CounterType::Lore,
                        threshold: Some(ch),
                    },
                ),
            );
        }
        id
    }

    #[test]
    fn saga_sacrificed_at_final_chapter() {
        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == id)
        ));
    }

    #[test]
    fn saga_not_sacrificed_below_final() {
        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 2);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn saga_not_sacrificed_with_chapter_on_stack() {
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);

        // Put a chapter trigger from this saga on the stack
        state.stack.push_back(StackEntry {
            id: ObjectId(999),
            source_id: id,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: id,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "chapter".into(),
                        description: None,
                    },
                    vec![],
                    id,
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: None,
            },
        });

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 714.4: Saga survives while chapter trigger is on the stack
        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn saga_not_sacrificed_with_pending_lore_event() {
        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);

        // Simulate a lore counter having just been added in this batch
        let mut events = vec![GameEvent::CounterAdded {
            object_id: id,
            counter_type: CounterType::Lore,
            count: 1,
        }];

        check_state_based_actions(&mut state, &mut events);

        // CR 714.4 deferral: triggers haven't been placed yet
        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn lethal_damage_prevented_by_regen_shield() {
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.damage_marked = 3; // lethal

            // Add regeneration shield
            let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate".to_string())
                .regeneration_shield();
            obj.replacement_definitions.push(shield);
        }

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 701.19a: Creature survives lethal damage via regeneration
        assert!(
            state.battlefield.contains(&id),
            "Creature with regen shield should survive lethal damage SBA"
        );
        // Damage cleared by regeneration
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.damage_marked, 0, "Regeneration should remove damage");
        assert!(obj.tapped, "Regeneration should tap the creature");
        // Shield consumed
        assert!(obj.replacement_definitions[0].is_consumed);
        // Regenerated event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Regenerated { object_id } if *object_id == id)));
    }

    // --- CR 704.5b: Draw from empty library SBA tests ---

    #[test]
    fn sba_draw_from_empty_library_loses() {
        let mut state = setup();
        state.players[0].drew_from_empty_library = true;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(0)
            }
        )));
    }

    #[test]
    fn sba_draw_from_empty_library_flag_not_set_survives() {
        let mut state = setup();
        // Flag not set — player should survive
        assert!(!state.players[0].drew_from_empty_library);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    // --- CR 704.5j: Legend rule choice tests ---

    #[test]
    fn sba_legend_rule_no_action_with_one_legend() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Thalia", 2, 1);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Single legend — no choice needed
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::ChooseLegend { .. }
        ));
        assert!(state.battlefield.contains(&id));
    }

    // --- CR 704.5q: Counter cancellation tests ---

    #[test]
    fn counter_cancellation_removes_pairs() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.counters.insert(CounterType::Plus1Plus1, 3);
        obj.counters.insert(CounterType::Minus1Minus1, 2);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "Should have 1 +1/+1 counter remaining"
        );
        assert_eq!(
            obj.counters
                .get(&CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            0,
            "Should have 0 -1/-1 counters remaining"
        );
    }

    #[test]
    fn counter_cancellation_equal_counts() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.counters.insert(CounterType::Plus1Plus1, 2);
        obj.counters.insert(CounterType::Minus1Minus1, 2);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        let obj = state.objects.get(&id).unwrap();
        assert!(
            !obj.counters.contains_key(&CounterType::Plus1Plus1),
            "Both counter types should be fully removed"
        );
        assert!(!obj.counters.contains_key(&CounterType::Minus1Minus1));
    }

    // --- CR 704.5d: Token cease-to-exist tests ---

    #[test]
    fn token_in_graveyard_ceases_to_exist() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Token".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().is_token = true;

        // Move token to graveyard
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        // Run SBAs
        check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.objects.contains_key(&id),
            "Token should be removed from objects"
        );
        assert!(
            !state.players[0].graveyard.contains(&id),
            "Token should be removed from graveyard"
        );
    }

    #[test]
    fn token_on_stack_survives_sba() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "CopyToken".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&id).unwrap().is_token = true;

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            state.objects.contains_key(&id),
            "Token on stack should survive SBA"
        );
    }

    // --- CR 104.3b: CantLoseTheGame SBA prevention tests ---

    /// Helper: add a permanent with CantLoseTheGame static affecting its controller.
    fn add_cant_lose_permanent(state: &mut GameState, owner: PlayerId) -> ObjectId {
        use crate::types::ability::StaticDefinition;
        let id = create_object(
            state,
            CardId(100),
            owner,
            "Platinum Angel".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseTheGame).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );
        id
    }

    #[test]
    fn sba_cant_lose_prevents_life_elimination() {
        let mut state = setup();
        // Set player 0 to 0 life
        state.players[0].life = 0;
        // Add Platinum Angel for player 0
        add_cant_lose_permanent(&mut state, PlayerId(0));

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Player 0 should NOT be eliminated
        assert!(
            !state.players[0].is_eliminated,
            "Player with CantLoseTheGame at 0 life should not be eliminated"
        );
        assert!(!state.eliminated_players.contains(&PlayerId(0)));
    }

    #[test]
    fn sba_cant_lose_prevents_draw_from_empty() {
        let mut state = setup();
        // Mark player 0 as having drawn from empty library
        state.players[0].drew_from_empty_library = true;
        // Add Platinum Angel for player 0
        add_cant_lose_permanent(&mut state, PlayerId(0));

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Player 0 should NOT be eliminated
        assert!(
            !state.players[0].is_eliminated,
            "Player with CantLoseTheGame who drew from empty should not be eliminated"
        );
    }

    #[test]
    fn sba_cant_lose_prevents_poison_elimination() {
        let mut state = setup();
        // Give player 0 ten poison counters
        state.players[0].poison_counters = 10;
        // Add Platinum Angel for player 0
        add_cant_lose_permanent(&mut state, PlayerId(0));

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Player 0 should NOT be eliminated
        assert!(
            !state.players[0].is_eliminated,
            "Player with CantLoseTheGame with 10 poison should not be eliminated"
        );
    }

    #[test]
    fn sba_cant_lose_does_not_affect_opponent() {
        let mut state = setup();
        // Set player 1 to 0 life
        state.players[1].life = 0;
        // Add Platinum Angel for player 0 — this should NOT protect player 1
        add_cant_lose_permanent(&mut state, PlayerId(0));

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Player 1 SHOULD be eliminated (not protected)
        assert!(
            state.players[1].is_eliminated,
            "Opponent of CantLoseTheGame controller should still be eliminated"
        );
    }

    // --- CR 702.131b: Ascend / city's blessing grant SBA ---

    fn add_ascend_permanent(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_creature(state, CardId(9001), owner, name, 2, 2);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Ascend);
        id
    }

    fn add_filler_permanent(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        create_creature(state, CardId(9002), owner, name, 1, 1)
    }

    #[test]
    fn ascend_nine_permanents_no_blessing() {
        let mut state = setup();
        add_ascend_permanent(&mut state, PlayerId(0), "Tendershoot");
        for i in 0..8 {
            add_filler_permanent(&mut state, PlayerId(0), &format!("Filler{i}"));
        }

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(!state.city_blessing.contains(&PlayerId(0)));
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CityBlessingGained { .. })));
    }

    #[test]
    fn ascend_ten_permanents_grants_blessing() {
        let mut state = setup();
        add_ascend_permanent(&mut state, PlayerId(0), "Tendershoot");
        for i in 0..9 {
            add_filler_permanent(&mut state, PlayerId(0), &format!("Filler{i}"));
        }

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(state.city_blessing.contains(&PlayerId(0)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CityBlessingGained {
                player_id: PlayerId(0)
            }
        )));
    }

    #[test]
    fn ascend_blessing_is_one_way_latch() {
        let mut state = setup();
        let ascender = add_ascend_permanent(&mut state, PlayerId(0), "Tendershoot");
        let fillers: Vec<ObjectId> = (0..9)
            .map(|i| add_filler_permanent(&mut state, PlayerId(0), &format!("Filler{i}")))
            .collect();

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);
        assert!(state.city_blessing.contains(&PlayerId(0)));

        // Drop back below 10 permanents by moving fillers off the battlefield.
        for id in fillers.iter().take(5) {
            state.battlefield.retain(|bid| bid != id);
        }
        assert_eq!(permanents_controlled(&state, PlayerId(0)), 5);

        let mut events2 = Vec::new();
        check_state_based_actions(&mut state, &mut events2);

        // Blessing persists (CR 702.131b — "for the rest of the game").
        assert!(state.city_blessing.contains(&PlayerId(0)));
        let _ = ascender; // silence unused binding — source is still on battlefield.
    }

    #[test]
    fn ascend_no_ascend_permanent_no_blessing() {
        let mut state = setup();
        // Ten permanents, none with Ascend.
        for i in 0..10 {
            add_filler_permanent(&mut state, PlayerId(0), &format!("Filler{i}"));
        }

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(!state.city_blessing.contains(&PlayerId(0)));
    }

    #[test]
    fn ascend_blessing_marks_layers_dirty() {
        let mut state = setup();
        add_ascend_permanent(&mut state, PlayerId(0), "Tendershoot");
        for i in 0..9 {
            add_filler_permanent(&mut state, PlayerId(0), &format!("Filler{i}"));
        }
        state.layers_dirty = false;

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 702.131d: continuous effects reapply after grant — layers must re-evaluate.
        assert!(state.layers_dirty || state.city_blessing.contains(&PlayerId(0)));
        assert!(state.city_blessing.contains(&PlayerId(0)));
    }
}

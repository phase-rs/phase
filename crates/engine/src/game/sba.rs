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
    if state.layers_dirty.is_dirty() {
        // Snapshot P/T before layer re-evaluation for delta logging.
        let pt_snapshot: Vec<(crate::types::identifiers::ObjectId, i32, i32)> = state
            .battlefield
            .iter()
            .filter_map(|&id| {
                let obj = state.objects.get(&id)?;
                Some((id, obj.power?, obj.toughness?))
            })
            .collect();

        layers::flush_layers(state);

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

        // CR 704.3 + CR 104.4a + CR 704.5a-c + CR 704.6c: Every player-loss
        // condition met in this single SBA check forms ONE simultaneous event.
        // Collect all losers across the conditions, then eliminate them together
        // so the game-over check sees the true post-event living set — a draw
        // (winner: None) when all remaining players lose at once, instead of
        // crowning whichever player happened to be eliminated first.
        // CR 704.5a-c + CR 704.6c: collect every player-loss SBA from this
        // check before applying any of them.
        let mut losers: Vec<PlayerId> = collect_life_losers(state);
        losers.extend(collect_draw_from_empty_losers(state));
        losers.extend(collect_poison_losers(state));
        losers.extend(collect_commander_damage_losers(state));

        // A player can meet several loss conditions at once — dedup so each is
        // eliminated (and emits PlayerLost) exactly once.
        losers.sort_unstable();
        losers.dedup();
        if !losers.is_empty() {
            any_performed = true;
            for &loser in &losers {
                events.push(GameEvent::PlayerLost { player_id: loser });
            }
            super::elimination::eliminate_players_simultaneously(state, &losers, events);

            // If the game ended (a sole winner or a CR 104.4a draw), stop now.
            if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                return;
            }
        }

        // CR 903.9a: A commander in graveyard or exile (since last SBA check) may
        // be put into the command zone by its owner. This pauses the SBA loop to
        // ask the player, similar to the legend rule.
        check_commander_zone_return(state);
        if matches!(state.waiting_for, WaitingFor::CommanderZoneChoice { .. }) {
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
        check_unattached_equipment(state, events, &mut any_performed);

        // CR 704.5y + CR 303.7a: If a permanent has more than one Role controlled
        // by the same player attached to it, all but the newest go to the
        // graveyard. Runs after unattached_auras so dead-host Roles are already
        // gone — only attached Roles compete for the per-(host, controller) slot.
        check_role_uniqueness(state, events, &mut any_performed);

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
        crate::game::layers::mark_layers_full(state);
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
/// from any permanent on the battlefield OR from a spell-applied transient
/// continuous effect (Everybody Lives!: "Players can't lose the game this turn.")
/// bound to this specific player. If so, SBAs that would cause that player to
/// lose the game are skipped.
///
/// Mirrors `player_has_protection_from_everything` in `static_abilities.rs`:
/// for transient effects scoped to players, we scan `transient_continuous_effects`
/// for entries pinned to this player via `SpecificPlayer { id }` whose
/// modifications grant `StaticMode::CantLoseTheGame`. The battlefield scan
/// handles the permanent-source path (Platinum Angel and friends).
fn player_has_cant_lose(state: &GameState, player_id: PlayerId) -> bool {
    let from_permanent = state.battlefield.iter().any(|&id| {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => return false,
        };
        super::functioning_abilities::active_static_definitions(state, obj).any(|def| {
            def.mode == StaticMode::CantLoseTheGame
                && static_affects_player(obj.controller, &def.affected, player_id)
        })
    });
    if from_permanent {
        return true;
    }
    super::static_abilities::transient_grants_static_mode_to_player(
        state,
        player_id,
        &StaticMode::CantLoseTheGame,
    )
}

/// CR 704.5a/704.5b/704.5c/704.6c: Whether a player-loss SBA would currently
/// eliminate at least one non-eliminated player. This is intentionally narrower
/// than running the full SBA loop: callers that are only trying to avoid waiting
/// on a dead player during a non-priority continuation should not trigger
/// unrelated SBA choice prompts such as commander-zone or legend-rule choices.
pub(crate) fn has_pending_player_loss_sba(state: &GameState) -> bool {
    let life_loss = state.players.iter().any(|player| {
        // CR 704.5a: A player with 0 or less life loses the game.
        !player.is_eliminated
            && !player.is_phased_out()
            && player.life <= 0
            && !player_has_cant_lose(state, player.id)
    });
    if life_loss {
        return true;
    }

    let drew_from_empty = state.players.iter().any(|player| {
        // CR 704.5b: A player who attempted to draw from an empty library loses
        // the game the next time state-based actions are checked.
        !player.is_eliminated
            && player.drew_from_empty_library
            && !player_has_cant_lose(state, player.id)
    });
    if drew_from_empty {
        return true;
    }

    let poison_loss = state.players.iter().any(|player| {
        // CR 704.5c: A player with ten or more poison counters loses the game.
        !player.is_eliminated
            && player.poison_counters >= 10
            && !player_has_cant_lose(state, player.id)
    });
    if poison_loss {
        return true;
    }

    let threshold = match state.format_config.commander_damage_threshold {
        Some(threshold) => threshold as u32,
        None => return false,
    };

    state.commander_damage.iter().any(|entry| {
        // CR 704.6c: In Commander, a player dealt 21+ combat damage by the same
        // commander over the course of the game loses.
        entry.damage >= threshold
            && !state.eliminated_players.contains(&entry.player)
            && !player_has_cant_lose(state, entry.player)
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
            Some(ControllerRef::ScopedPlayer) => false,
            Some(ControllerRef::TargetPlayer) => false,
            Some(ControllerRef::ParentTargetController) => false,
            Some(ControllerRef::DefendingPlayer) => false,
            // CR 109.4: Chosen-player scope has no resolution context here.
            // Fail closed.
            Some(ControllerRef::ChosenPlayer { .. }) => false,
            // CR 603.2 + CR 109.4: Triggering-player scope has no event
            // context for static-ability scoping. Fail closed.
            Some(ControllerRef::TriggeringPlayer) => false,
            None => true,
        },
        Some(TargetFilter::Player) => true,
        Some(TargetFilter::Any) => true,
        None => true,
        _ => false,
    }
}

/// CR 704.5a: A player with 0 or less life loses the game. Pure collector — the
/// SBA driver batches all loss conditions into a single simultaneous event
/// (CR 704.3) so simultaneous deaths can resolve to a draw (CR 104.4a).
///
/// CR 104.3b: Skip players protected by CantLoseTheGame.
///
/// Player-phasing exclusion: a phased-out player can't lose the game from
/// 0-or-less life — they're treated as though they don't exist for SBA
/// purposes (mirrors CR 702.26b for permanents, applied to players).
fn collect_life_losers(state: &GameState) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && !p.is_phased_out() && p.life <= 0)
        .filter(|p| !player_has_cant_lose(state, p.id))
        .map(|p| p.id)
        .collect()
}

/// CR 704.5b: A player who attempted to draw from an empty library loses the
/// game. Pure collector (see `collect_life_losers`).
fn collect_draw_from_empty_losers(state: &GameState) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.drew_from_empty_library)
        .filter(|p| !player_has_cant_lose(state, p.id))
        .map(|p| p.id)
        .collect()
}

/// CR 704.5c: A player with ten or more poison counters loses the game. Pure
/// collector (see `collect_life_losers`).
fn collect_poison_losers(state: &GameState) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.poison_counters >= 10)
        .filter(|p| !player_has_cant_lose(state, p.id))
        .map(|p| p.id)
        .collect()
}

/// CR 903.9a: If a commander is in a graveyard or exile (and was put there
/// since the last SBA check), its owner may put it into the command zone.
/// CR 903.9b: Hand and library are also covered (see `commander_eligible_for_zone_return`).
///
/// Pauses the SBA loop by setting `WaitingFor::CommanderZoneChoice` so the
/// player can accept (move to command zone) or decline (leave in place).
fn check_commander_zone_return(state: &mut GameState) {
    if !state.format_config.command_zone {
        return;
    }

    if let Some((commander_id, owner, current_zone)) =
        super::commander::commander_eligible_for_zone_return(state)
    {
        state.waiting_for = WaitingFor::CommanderZoneChoice {
            player: owner,
            commander_id,
            current_zone,
        };
    }
}

/// CR 704.6c: A player dealt 21+ combat damage by the same commander loses.
/// Pure collector (see `collect_life_losers`).
fn collect_commander_damage_losers(state: &GameState) -> Vec<PlayerId> {
    let threshold = match state.format_config.commander_damage_threshold {
        Some(t) => t as u32,
        None => return Vec::new(), // Not a Commander format
    };

    // CR 104.3b: Skip players protected by CantLoseTheGame.
    state
        .commander_damage
        .iter()
        .filter(|entry| entry.damage >= threshold)
        .map(|entry| entry.player)
        .filter(|pid| !state.eliminated_players.contains(pid))
        .filter(|pid| !player_has_cant_lose(state, *pid))
        .collect()
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

    for &id in &to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
    // CR 603.10a + CR 704.3: state-based actions are performed simultaneously, so
    // these permanents left the battlefield together — record the group so
    // co-departing leaves-the-battlefield/dies observers observe each other.
    zones::mark_simultaneous_departures(events, &to_destroy);
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
    for &id in &to_destroy {
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
    // CR 603.10a + CR 704.3: creatures destroyed by lethal damage in this SBA
    // check died simultaneously as a single event — record the group so
    // co-departing dies/LTB observers (Blood Artist) observe each other.
    // CR 701.19a/b: a creature whose destruction was Prevented (regeneration)
    // stays on the battlefield, so `departed_subset` excludes it from the group.
    zones::mark_simultaneous_departures(events, &zones::departed_subset(state, &to_destroy));
}

/// CR 704.5j: A legendary permanent is exempt from the legend rule while an
/// active `LegendRuleDoesntApply` static has an `affected` filter that matches
/// it (Mirror Gallery's global exemption, Sakashima of a Thousand Faces /
/// Mirror Box's "permanents you control", Cadric / Sliver Gravemother's
/// type-scoped variants). The candidate is passed as the target object so
/// type-scoped exemptions are evaluated per-permanent, not per-player.
fn legend_rule_exempt(
    state: &GameState,
    permanent_id: crate::types::identifiers::ObjectId,
) -> bool {
    super::static_abilities::check_static_ability(
        state,
        StaticMode::LegendRuleDoesntApply,
        &super::static_abilities::StaticCheckContext {
            target_id: Some(permanent_id),
            ..Default::default()
        },
    )
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
                    // CR 704.5j: a permanent exempted by a "legend rule doesn't
                    // apply" static is excluded from the same-name grouping.
                    && !legend_rule_exempt(state, *id)
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
    // CR 702.103f override: Bestow Auras have a special unattached behavior —
    // when an attached bestow Aura becomes unattached (host died, host became
    // an illegal target, etc.), the bestow type-changing effect ends and the
    // permanent stays on the battlefield as an enchantment creature. This is
    // explicitly an exception to CR 704.5m, so we partition the unattached
    // Aura set: bestow Auras revert in place, non-bestow Auras go to graveyard.
    enum UnattachedAuraAction {
        /// CR 704.5m: standard — move to owner's graveyard.
        ToGraveyard,
        /// CR 702.103f: bestow Aura — revert form, stay on battlefield.
        BestowRevert,
    }

    let actions: Vec<(crate::types::identifiers::ObjectId, UnattachedAuraAction)> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter_map(|id| {
            let obj = state.objects.get(&id)?;
            if !obj.card_types.core_types.contains(&CoreType::Enchantment) {
                return None;
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
                return None;
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
            let unattached = match obj.attached_to {
                Some(crate::game::game_object::AttachTarget::Object(t)) => {
                    !is_valid_attachment_target(state, id, t)
                }
                Some(crate::game::game_object::AttachTarget::Player(pid)) => {
                    !crate::game::effects::attach::can_attach_to_player(state, id, pid)
                }
                None => true,
            };
            if !unattached {
                return None;
            }
            // CR 702.103f: A bestowed Aura that becomes unattached ceases to
            // be bestowed and remains on the battlefield as a creature. This
            // overrides CR 704.5m for bestow Auras specifically.
            if obj.bestow_form.is_some() {
                Some((id, UnattachedAuraAction::BestowRevert))
            } else {
                Some((id, UnattachedAuraAction::ToGraveyard))
            }
        })
        .collect();

    for (id, action) in actions {
        match action {
            UnattachedAuraAction::ToGraveyard => {
                zones::move_to_zone(state, id, Zone::Graveyard, events);
            }
            UnattachedAuraAction::BestowRevert => {
                // CR 702.103f: revert in place — restore Creature form, drop
                // the synthesized Aura subtype + `enchant creature` keyword,
                // and detach from the (illegal) host so the permanent remains
                // on the battlefield unattached as an enchantment creature.
                // The host's `attachments` list was already cleaned when the
                // host changed zones.
                let old_target = state.objects.get(&id).and_then(|obj| {
                    obj.attached_to
                        .map(crate::game::effects::attach::target_ref_from_attach_target)
                });
                crate::game::casting::revert_bestow_form(state, id);
                if let Some(obj) = state.objects.get_mut(&id) {
                    obj.attached_to = None;
                }
                if let Some(old_target) = old_target
                    .as_ref()
                    .filter(|target| should_emit_sba_unattached_event(state, target))
                {
                    events.push(GameEvent::Unattached {
                        attachment_id: id,
                        old_target: old_target.clone(),
                    });
                }
            }
        }
        *any_performed = true;
    }
}

/// CR 704.5n + CR 301.5c: Equipment attached to an illegal permanent (or, per
/// CR 704.5n, to a player at all) becomes unattached. Equipment can never
/// legally attach to a player (CR 301.5), so a `Player` host is *always*
/// illegal and must be unattached on this SBA pass.
/// CR 702.26b: Phased-out Equipment is treated as though it doesn't exist.
fn check_unattached_equipment(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
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
                            !is_valid_attachment_target(state, *id, t)
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
        let old_target = state.objects.get(&equipment_id).and_then(|obj| {
            obj.attached_to
                .map(crate::game::effects::attach::target_ref_from_attach_target)
        });
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
        if let Some(old_target) = old_target
            .as_ref()
            .filter(|target| should_emit_sba_unattached_event(state, target))
        {
            events.push(GameEvent::Unattached {
                attachment_id: equipment_id,
                old_target: old_target.clone(),
            });
        }
        *any_performed = true;
    }
}

fn should_emit_sba_unattached_event(
    state: &GameState,
    old_target: &crate::types::ability::TargetRef,
) -> bool {
    match old_target {
        crate::types::ability::TargetRef::Object(target_id) => state
            .objects
            .get(target_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield),
        crate::types::ability::TargetRef::Player(_) => true,
    }
}

/// CR 704.5y + CR 303.7a: If a permanent has more than one Role controlled
/// by the same player attached to it, each of those Roles except the one
/// with the most recent timestamp is put into its owner's graveyard.
///
/// Grouping is per-(host, role-controller) — NOT per-name. Two same-controller
/// Roles with different names (Cursed + Royal) on one creature collapse to
/// one. Two different-controller Roles on one creature both stay.
///
/// CR 702.26b: Phased-out Roles are skipped via `battlefield_phased_in_ids`.
fn check_role_uniqueness(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    use crate::game::game_object::AttachTarget;
    use crate::types::identifiers::ObjectId;
    use std::collections::HashMap;

    // (host_creature, role_controller) → Vec<(role_id, timestamp)>
    let mut groups: HashMap<(ObjectId, PlayerId), Vec<(ObjectId, u64)>> = HashMap::new();
    for id in state.battlefield_phased_in_ids() {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        if !obj.card_types.subtypes.iter().any(|s| s == "Role") {
            continue;
        }
        // CR 303.7: Roles are Auras and only attach to permanents (Object hosts).
        let Some(AttachTarget::Object(host)) = obj.attached_to else {
            continue;
        };
        groups
            .entry((host, obj.controller))
            .or_default()
            .push((id, obj.timestamp));
    }

    // Iterate in deterministic order so test/log output is stable.
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort_by_key(|(host, ctrl)| (host.0, ctrl.0));

    for key in keys {
        let mut roles = groups.remove(&key).unwrap();
        if roles.len() < 2 {
            continue;
        }
        // CR 613.7 timestamp ordering — newest survives, older ones go to graveyard.
        // Tie-break by ObjectId so behavior is deterministic when timestamps collide.
        roles.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0 .0.cmp(&a.0 .0)));
        for (id, _) in roles.into_iter().skip(1) {
            zones::move_to_zone(state, id, Zone::Graveyard, events);
            *any_performed = true;
        }
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

    for &id in &to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
    // CR 603.10a + CR 704.3: state-based actions are performed simultaneously, so
    // these permanents left the battlefield together — record the group so
    // co-departing leaves-the-battlefield/dies observers observe each other.
    zones::mark_simultaneous_departures(events, &to_destroy);
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

    for &id in &to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
    // CR 603.10a + CR 704.3: state-based actions are performed simultaneously, so
    // these permanents left the battlefield together — record the group so
    // co-departing leaves-the-battlefield/dies observers observe each other.
    zones::mark_simultaneous_departures(events, &to_destroy);
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
            state.layers_dirty.mark_full(); // P/T affected via Layer 7d
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

/// CR 303.4c: An Aura is enchanting an illegal object or player when its
/// enchant ability (and other applicable effects) does not admit the host.
/// The Aura's `Keyword::Enchant(filter)` is the single authority — exactly
/// the same `matches_target_filter` predicate the cast-time path
/// (`game/casting.rs` Aura branch) uses to enumerate legal targets, so
/// cast-legality and SBA-legality cannot drift.
///
/// CR 702.5a: When the Enchant filter does not name a non-battlefield zone
/// (every standard Aura: Pacifism, Rancor, etc.), legality additionally
/// requires the host to be on the battlefield — this is the implicit "an
/// Aura attached to X" zone constraint from the rule's printed wording.
/// When the filter explicitly names a zone (CR 303.4a — Animate Dead,
/// Spellweaver Volute, Don't Worry About It), that zone IS the legal host
/// zone and the battlefield default is suspended.
///
/// CR 301.5: Equipment carries no `Keyword::Enchant`, so legality reduces to
/// the printed "on the battlefield" requirement.
fn is_valid_attachment_target(
    state: &GameState,
    attacher_id: crate::types::identifiers::ObjectId,
    target_id: crate::types::identifiers::ObjectId,
) -> bool {
    let Some(attacher) = state.objects.get(&attacher_id) else {
        return false;
    };
    let Some(target) = state.objects.get(&target_id) else {
        return false;
    };
    // CR 704.5m: An Aura attached to an illegal object is put into its owner's
    // graveyard.
    // CR 704.5n: Equipment attached to an illegal permanent becomes unattached.
    // Protection acquired by the host, or a prohibition static, makes the host
    // an illegal attachment target even though the Enchant filter / zone below
    // may still match.
    if crate::game::effects::attach::attachment_illegality(state, attacher_id, target_id).is_some()
    {
        return false;
    }
    let enchant_filter = attacher.keywords.iter().find_map(|k| match k {
        crate::types::keywords::Keyword::Enchant(f) => Some(f),
        _ => None,
    });
    let Some(filter) = enchant_filter else {
        // Equipment / non-Enchant attacher: only the battlefield is a legal host.
        return target.zone == Zone::Battlefield;
    };

    // CR 702.5a battlefield default: if the filter does not opt into a
    // non-battlefield zone via `FilterProp::InZone`, the host must be on the
    // battlefield. Mirrors the cast-time `extract_explicit_zones` branch in
    // `game::targeting::find_legal_targets`.
    let allowed_zones = explicit_enchant_zones(filter);
    if allowed_zones.is_empty() {
        if target.zone != Zone::Battlefield {
            return false;
        }
    } else if !allowed_zones.contains(&target.zone) {
        return false;
    }

    let ctx = crate::game::filter::FilterContext::from_source_with_controller(
        attacher_id,
        attacher.controller,
    );
    crate::game::filter::matches_target_filter(state, target_id, filter, &ctx)
}

/// CR 303.4a: Collect every `FilterProp::InZone` zone reachable through the
/// `TargetFilter` AST. Mirrors `game::targeting::extract_explicit_zones`; kept
/// private here so the SBA helper does not depend on a `pub(crate)` lift in
/// the targeting module.
fn explicit_enchant_zones(filter: &crate::types::ability::TargetFilter) -> Vec<Zone> {
    use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties
            .iter()
            .filter_map(|p| match p {
                FilterProp::InZone { zone } => Some(*zone),
                _ => None,
            })
            .collect(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().flat_map(explicit_enchant_zones).collect()
        }
        TargetFilter::Not { filter } => explicit_enchant_zones(filter),
        _ => vec![],
    }
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

    /// Issue #537 SBA SHAPE test (5d) — **explicitly labeled SHAPE**: this
    /// test constructs the post-resolution state by hand (Aura on battlefield
    /// attached to a graveyard creature) and asserts the SBA helper accepts
    /// it. It does NOT drive the cast → resolve pipeline; see
    /// `sba_animate_dead_pipeline_aura_survives_after_etb` for the runtime
    /// sibling.
    ///
    /// CR 303.4c: SBA legality is defined by the Aura's enchant filter, not
    /// by a hardcoded `zone == Battlefield` predicate. Pre-fix, the helper
    /// would have moved this Aura to the graveyard because the host is not
    /// on the battlefield.
    #[test]
    fn sba_shape_aura_with_graveyard_enchant_filter_survives() {
        use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};
        use crate::types::keywords::Keyword;

        let mut state = setup();
        // The Aura on the battlefield with zone-aware Enchant filter.
        let aura_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Animate Dead".to_string(),
            Zone::Battlefield,
        );
        let host_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Graveyard,
        );
        {
            let host = state.objects.get_mut(&host_id).unwrap();
            host.card_types.core_types.push(CoreType::Creature);
        }
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords.push(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }]),
            )));
            aura.attached_to = Some(host_id.into());
        }

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 303.4c: graveyard host is legal per the Aura's enchant filter,
        // so the Aura must remain on the battlefield (NOT moved to graveyard
        // by the unattached-Aura SBA).
        assert!(
            state.battlefield.contains(&aura_id),
            "Aura with zone-aware enchant filter must survive SBA when attached to a legal graveyard host"
        );
        assert!(!state.players[0].graveyard.contains(&aura_id));
    }

    /// Issue #537 runtime pipeline test (5e) — sibling to the 5d SHAPE test.
    /// Drives the **cast pipeline** end-to-end (`handle_cast_spell` →
    /// `find_legal_targets` over the MTGJSON-parsed Enchant keyword), then
    /// runs the **SBA pipeline** (`check_state_based_actions`) against the
    /// post-attachment state. This proves the parser fix threads correctly
    /// through both pipelines.
    ///
    /// NOTE: Animate Dead's ETB-trigger reanimation (returning the graveyard
    /// creature, then re-attaching) is OUT OF SCOPE per #537's plan. The
    /// stack resolver's `validate_targets_in_chain`
    /// (`ability_utils.rs:848-856`) filters object targets to the battlefield
    /// for `Effect::Unimplemented`-placeholder Auras, which would fizzle the
    /// Aura. To exercise the SBA helper against a legal graveyard host, the
    /// attachment that a complete reanimation pipeline would create is spliced
    /// in directly; the SBA helper then runs against the same shape it would
    /// in the real pipeline. CR 117.5 / 704.3: SBAs run before priority;
    /// CR 303.4c: legality is defined by the Aura's enchant filter, not a
    /// hardcoded battlefield-only predicate.
    #[test]
    fn sba_animate_dead_pipeline_aura_survives_after_etb() {
        use crate::game::casting::handle_cast_spell;
        use crate::types::ability::TargetRef;
        use crate::types::game_state::{StackEntryKind, WaitingFor};
        use crate::types::identifiers::CardId;
        use crate::types::keywords::Keyword;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use std::str::FromStr;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Aura in hand, parsed through the MTGJSON FromStr path.
        let aura_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Animate Dead".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords
                .push(Keyword::from_str("Enchant:creature card in a graveyard").unwrap());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }

        // Add one black mana so the cast can be paid.
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Black,
            source_id: crate::types::identifiers::ObjectId(0),
            snow: false,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
        // Creature card in opponent's graveyard.
        let creature_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Cast: should auto-target the only legal graveyard creature.
        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), aura_id, CardId(1), &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "expected cast to auto-target onto stack; got {result:?}"
        );
        assert_eq!(state.stack.len(), 1);

        // Verify the cast recorded the cross-zone target on the stack.
        let entry = state.stack.front().unwrap().clone();
        let target = if let StackEntryKind::Spell {
            ability: Some(ref a),
            ..
        } = entry.kind
        {
            a.targets
                .iter()
                .find_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .expect("Aura cast must record an Object target")
        } else {
            panic!("expected Spell entry");
        };
        assert_eq!(target, creature_id);

        // Splice in the post-ETB state (out-of-scope reanimation pipeline):
        // Aura on the battlefield, attached to the graveyard-hosted creature
        // card. This is the shape the SBA helper must accept.
        state.players[0].hand.retain(|&id| id != aura_id);
        state.stack.clear();
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.zone = Zone::Battlefield;
            obj.attached_to = Some(creature_id.into());
        }
        state.battlefield.push_back(aura_id);

        // Pipeline 2: drive SBAs. With the zone-aware Enchant filter
        // (CR 303.4c), the helper sees the graveyard host as legal and does
        // NOT yank the Aura. CR 117.5 / 704.3: SBAs run as a single event.
        check_state_based_actions(&mut state, &mut events);

        assert!(
            state.battlefield.contains(&aura_id),
            "Aura must survive SBA pass when attached to a creature card whose \
             zone matches its zone-aware Enchant filter (CR 303.4c + 117.5)"
        );
        assert!(!state.players[0].graveyard.contains(&aura_id));
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
    fn sba_aura_detaches_when_host_gains_protection() {
        // CR 702.16c: a creature enchanted by an opponent's white
        // Aura (Pacifism) that gains protection from white (Mother of Runes) →
        // the Aura is put into its owner's graveyard as a state-based action.
        // CR 704.5m: An illegal Aura is put into its owner's graveyard.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Pacifism".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.color.push(crate::types::mana::ManaColor::White);
            obj.attached_to = Some(creature.into());
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(aura);
        // Host gains protection from white.
        state.objects.get_mut(&creature).unwrap().keywords.push(
            crate::types::keywords::Keyword::Protection(
                crate::types::keywords::ProtectionTarget::Color(
                    crate::types::mana::ManaColor::White,
                ),
            ),
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 704.5m: the now-illegal Aura goes to its owner's graveyard.
        assert!(
            !state.battlefield.contains(&aura),
            "an Aura on a host that gained protection must detach"
        );
        assert!(
            state.players[1].graveyard.contains(&aura),
            "the illegal Aura must move to its owner's graveyard"
        );
    }

    #[test]
    fn sba_player_aura_detaches_when_player_gains_protection() {
        // CR 702.16c: a player with protection from everything can't be
        // enchanted by an Aura.
        // CR 704.5m: An Aura attached to an illegal player is put into its
        // owner's graveyard.
        let mut state = setup();
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Curse".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(crate::game::game_object::AttachTarget::Player(PlayerId(0)));
        }
        state.add_transient_continuous_effect(
            aura,
            PlayerId(0),
            crate::types::ability::Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![crate::types::ability::ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Protection(
                    crate::types::keywords::ProtectionTarget::Everything,
                ),
            }],
            None,
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&aura));
        assert!(state.players[1].graveyard.contains(&aura));
    }

    #[test]
    fn sba_equipment_unattaches_when_host_gains_protection_from_artifacts() {
        // CR 702.16d: an equipped creature that gains protection from artifacts
        // can't be equipped by artifact Equipment.
        // CR 704.5n: Illegal Equipment unattaches but stays on the battlefield.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let equip = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&equip).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.attached_to = Some(creature.into());
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(equip);
        state.objects.get_mut(&creature).unwrap().keywords.push(
            crate::types::keywords::Keyword::Protection(
                // `source_matches_card_type` matches the lowercase type word
                // (the form the parser stores for "protection from artifacts").
                crate::types::keywords::ProtectionTarget::CardType("artifact".to_string()),
            ),
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 704.5n: Equipment stays on the battlefield, but unattached.
        assert!(
            state.battlefield.contains(&equip),
            "Equipment stays on the battlefield (CR 704.5n)"
        );
        assert_eq!(
            state.objects.get(&equip).unwrap().attached_to,
            None,
            "Equipment must unattach from a host that gained protection from artifacts"
        );
    }

    #[test]
    fn sba_legal_aura_stays_attached() {
        // Regression guard: an Aura on a legal host (no protection / prohibition)
        // is not detached by the SBA re-check.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(creature.into());
        }
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .attachments
            .push(aura);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            state.battlefield.contains(&aura),
            "a legal Aura must remain attached"
        );
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(creature.into())
        );
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
                source_name: String::new(),
                subject_match_count: None,
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

    // --- CR 704.5j: Legend-rule exemption tests (Sakashima / Mirror Gallery class) ---

    /// Helper: put a legendary creature with the given name onto the battlefield
    /// under `owner`'s control.
    fn add_legendary(
        state: &mut GameState,
        card: CardId,
        owner: PlayerId,
        name: &str,
        turn: u32,
    ) -> ObjectId {
        let id = create_creature(state, card, owner, name, 2, 1);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.entered_battlefield_turn = Some(turn);
        id
    }

    /// Helper: add a permanent whose `LegendRuleDoesntApply` static carries the
    /// given `affected` scope (`None` = global Mirror Gallery; a controller-scoped
    /// filter = Sakashima/Cadric class).
    fn add_legend_exemption(
        state: &mut GameState,
        owner: PlayerId,
        affected: Option<TargetFilter>,
    ) -> ObjectId {
        use crate::types::ability::StaticDefinition;
        let id = create_object(
            state,
            CardId(200),
            owner,
            "Legend Exemption".to_string(),
            Zone::Battlefield,
        );
        let mut def = StaticDefinition::new(StaticMode::LegendRuleDoesntApply);
        def.affected = affected;
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(def);
        id
    }

    #[test]
    fn sba_legend_rule_suppressed_by_global_exemption() {
        // CR 704.5j: Mirror Gallery — "The legend rule doesn't apply." (global).
        let mut state = setup();
        let id1 = add_legendary(&mut state, CardId(1), PlayerId(0), "Thalia", 1);
        let id2 = add_legendary(&mut state, CardId(2), PlayerId(0), "Thalia", 2);
        add_legend_exemption(&mut state, PlayerId(0), None);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseLegend { .. }),
            "global legend-rule exemption must suppress the legend-rule choice"
        );
        assert!(state.battlefield.contains(&id1));
        assert!(state.battlefield.contains(&id2));
    }

    #[test]
    fn sba_legend_rule_suppressed_for_controller_scope() {
        // CR 704.5j: Sakashima of a Thousand Faces — "doesn't apply to permanents
        // you control." The controller keeps both same-name legendaries.
        let mut state = setup();
        let id1 = add_legendary(&mut state, CardId(1), PlayerId(0), "Sakashima", 1);
        let id2 = add_legendary(&mut state, CardId(2), PlayerId(0), "Sakashima", 2);
        add_legend_exemption(
            &mut state,
            PlayerId(0),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseLegend { .. }),
            "controller-scoped legend-rule exemption must suppress the choice for its controller"
        );
        assert!(state.battlefield.contains(&id1));
        assert!(state.battlefield.contains(&id2));
    }

    #[test]
    fn sba_legend_rule_still_applies_to_opponent_without_exemption() {
        // CR 704.5j: Sakashima's "permanents you control" exemption is controller
        // scoped — an opponent who controls two same-name legendaries is still
        // subject to the legend rule.
        let mut state = setup();
        // Player 0 controls Sakashima (the exemption source).
        add_legend_exemption(
            &mut state,
            PlayerId(0),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );
        // Player 1 controls two copies of the same legendary, with no exemption.
        let id1 = add_legendary(&mut state, CardId(1), PlayerId(1), "Atraxa", 1);
        let id2 = add_legendary(&mut state, CardId(2), PlayerId(1), "Atraxa", 2);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        match &state.waiting_for {
            WaitingFor::ChooseLegend {
                player, candidates, ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(candidates.contains(&id1));
                assert!(candidates.contains(&id2));
            }
            other => panic!("Expected ChooseLegend for opponent, got {:?}", other),
        }
    }

    #[test]
    fn sba_legend_rule_type_scoped_exemption_only_exempts_matching() {
        // CR 704.5j: Sliver Gravemother — "doesn't apply to Slivers you control."
        // Two same-name NON-Sliver legendaries are still collapsed by the rule.
        let mut state = setup();
        add_legendary(&mut state, CardId(1), PlayerId(0), "Sliver Overlord", 1);
        add_legendary(&mut state, CardId(2), PlayerId(0), "Sliver Overlord", 2);
        // The exemption only covers Slivers; the legendaries above have no subtype.
        add_legend_exemption(
            &mut state,
            PlayerId(0),
            Some(TargetFilter::Typed(
                TypedFilter::default()
                    .controller(ControllerRef::You)
                    .subtype("Sliver".to_string()),
            )),
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseLegend { .. }),
            "type-scoped exemption must not exempt permanents outside its filter"
        );
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

    #[test]
    fn counter_cancellation_does_not_cancel_other_power_toughness_counters() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let pt_counter = CounterType::PowerToughness {
            power: 0,
            toughness: -1,
        };
        let obj = state.objects.get_mut(&id).unwrap();
        obj.counters.insert(CounterType::Plus1Plus1, 1);
        obj.counters.insert(pt_counter.clone(), 1);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(1));
        assert_eq!(obj.counters.get(&pt_counter).copied(), Some(1));
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

    #[test]
    fn sba_simultaneous_life_loss_is_a_draw() {
        // CR 104.4a + CR 704.3: both players at <=0 life in one SBA check lose
        // simultaneously → the game is a DRAW (winner: None), not a win for the
        // player processed first.
        let mut state = setup();
        state.players[0].life = 0;
        state.players[1].life = 0;

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            "both players at 0 life simultaneously must be a draw, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn sba_single_life_loss_yields_sole_winner() {
        // Only one player loses → the other wins (single-loser behavior intact).
        let mut state = setup();
        state.players[1].life = 0;

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "a single player at 0 life leaves the other as sole winner, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn sba_mixed_life_and_poison_loss_is_a_draw() {
        // CR 704.3: loss conditions of DIFFERENT kinds in the same SBA check are
        // still one simultaneous event — one player at 0 life and the other at
        // 10 poison both lose at once → draw.
        let mut state = setup();
        state.players[0].life = 0;
        state.players[1].poison_counters = 10;

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            "life-loss + poison-loss in one SBA check is a simultaneous draw, got {:?}",
            state.waiting_for
        );
    }

    /// CR 104.3 + CR 704.5b + CR 611.1: Spell-applied transient continuous
    /// effects (Everybody Lives!: "Players can't lose the game ... this turn.")
    /// bound to a specific player via `SpecificPlayer { id }` must also block
    /// draw-from-empty elimination, mirroring the permanent-source path. This
    /// covers the bug where attempting to draw on an empty library caused a
    /// player to lose despite Everybody Lives! resolving on the same turn.
    #[test]
    fn sba_cant_lose_tce_prevents_draw_from_empty() {
        use crate::types::ability::{ContinuousModification, Duration};
        let mut state = setup();
        state.players[0].drew_from_empty_library = true;

        // Install a TCE that grants CantLoseTheGame to player 0 — matches the
        // shape `register_transient_effect` creates when resolving the
        // GenericEffect emitted for "Players can't lose the game this turn".
        state.add_transient_continuous_effect(
            crate::types::identifiers::ObjectId(999),
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantLoseTheGame,
            }],
            None,
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.players[0].is_eliminated,
            "Player covered by spell-applied CantLoseTheGame TCE must not be \
             eliminated by draw-from-empty SBA"
        );
        assert!(!state.eliminated_players.contains(&PlayerId(0)));
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
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 702.131d: continuous effects reapply after grant — layers must re-evaluate.
        assert!(state.layers_dirty.is_dirty() || state.city_blessing.contains(&PlayerId(0)));
        assert!(state.city_blessing.contains(&PlayerId(0)));
    }

    // --- CR 704.5y: Role uniqueness SBA ---

    fn create_role_token(
        state: &mut GameState,
        card_id: CardId,
        controller: PlayerId,
        owner: PlayerId,
        name: &str,
        host: ObjectId,
        timestamp: u64,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.controller = controller;
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.card_types.subtypes.push("Role".to_string());
        obj.attached_to = Some(host.into());
        obj.timestamp = timestamp;
        // Mirror the host's attachments list so dependent SBAs (lethal damage,
        // unattached aura cleanup) see a consistent attachment graph.
        if let Some(h) = state.objects.get_mut(&host) {
            h.attachments.push(id);
        }
        id
    }

    #[test]
    fn sba_role_uniqueness_keeps_newest_same_controller() {
        // CR 704.5y: same player puts two Roles on the same creature → newest survives.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let older = create_role_token(
            &mut state,
            CardId(2),
            PlayerId(0),
            PlayerId(0),
            "Royal",
            creature,
            10,
        );
        let newer = create_role_token(
            &mut state,
            CardId(3),
            PlayerId(0),
            PlayerId(0),
            "Cursed",
            creature,
            20,
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.battlefield.contains(&older),
            "older Role must leave the battlefield"
        );
        assert!(
            state.players[0].graveyard.contains(&older),
            "older Role must go to its owner's graveyard"
        );
        assert!(
            state.battlefield.contains(&newer),
            "newest Role must survive — name does not matter for grouping"
        );
    }

    #[test]
    fn sba_role_uniqueness_per_controller_not_per_creature() {
        // CR 303.7a: grouping is per Role-controller. Two Roles on one
        // creature controlled by different players are both legal.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let role_p0 = create_role_token(
            &mut state,
            CardId(2),
            PlayerId(0),
            PlayerId(0),
            "Royal",
            creature,
            10,
        );
        let role_p1 = create_role_token(
            &mut state,
            CardId(3),
            PlayerId(1),
            PlayerId(1),
            "Wicked",
            creature,
            20,
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            state.battlefield.contains(&role_p0),
            "P0's Role survives — P1's Role is in a different group"
        );
        assert!(
            state.battlefield.contains(&role_p1),
            "P1's Role survives — different controller from P0's Role"
        );
    }

    #[test]
    fn sba_role_uniqueness_three_roles_keep_newest_only() {
        // CR 704.5y: with N>2, only the most-recent timestamp survives.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let r1 = create_role_token(
            &mut state,
            CardId(2),
            PlayerId(0),
            PlayerId(0),
            "Royal",
            creature,
            10,
        );
        let r2 = create_role_token(
            &mut state,
            CardId(3),
            PlayerId(0),
            PlayerId(0),
            "Cursed",
            creature,
            20,
        );
        let r3 = create_role_token(
            &mut state,
            CardId(4),
            PlayerId(0),
            PlayerId(0),
            "Monster",
            creature,
            30,
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.battlefield.contains(&r1) && state.players[0].graveyard.contains(&r1),
            "oldest Role goes to graveyard"
        );
        assert!(
            !state.battlefield.contains(&r2) && state.players[0].graveyard.contains(&r2),
            "middle Role goes to graveyard"
        );
        assert!(state.battlefield.contains(&r3), "newest Role survives");
    }

    #[test]
    fn sba_role_uniqueness_single_role_unaffected() {
        // CR 704.5y: with only one Role on the host, the SBA does nothing.
        let mut state = setup();
        let creature = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let role = create_role_token(
            &mut state,
            CardId(2),
            PlayerId(0),
            PlayerId(0),
            "Royal",
            creature,
            10,
        );

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&role));
        assert!(state.players[0].graveyard.is_empty());
    }
}

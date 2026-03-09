use std::collections::HashMap;
use std::str::FromStr;

use crate::types::ability::{ResolvedAbility, TriggerDefinition};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::stack;

/// Function signature for trigger matchers: returns true if event matches the trigger.
pub type TriggerMatcher = fn(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool;

/// A trigger that matched an event and is waiting to be placed on the stack.
#[derive(Debug, Clone)]
pub struct PendingTrigger {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub trigger_def: TriggerDefinition,
    pub ability: ResolvedAbility,
    pub timestamp: u32,
}

/// Process events and place triggered abilities on the stack in APNAP order.
pub fn process_triggers(state: &mut GameState, events: &[GameEvent]) {
    let registry = build_trigger_registry();
    let mut pending: Vec<PendingTrigger> = Vec::new();

    for event in events {
        // Scan all permanents on the battlefield for matching triggers
        let battlefield_ids: Vec<ObjectId> = state.battlefield.clone();
        for obj_id in battlefield_ids {
            let (controller, trigger_defs, timestamp, svars, has_prowess) = {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                (
                    obj.controller,
                    obj.trigger_definitions.clone(),
                    obj.entered_battlefield_turn.unwrap_or(0),
                    obj.svars.clone(),
                    obj.has_keyword(&Keyword::Prowess),
                )
            };

            for trig_def in &trigger_defs {
                let mode = TriggerMode::from_str(&trig_def.mode)
                    .unwrap_or(TriggerMode::Unknown(trig_def.mode.clone()));

                if let Some(matcher) = registry.get(&mode) {
                    if matcher(event, &trig_def.params, obj_id, state) {
                        // Build the ResolvedAbility from the trigger definition
                        let ability = build_triggered_ability(trig_def, obj_id, controller, &svars);
                        pending.push(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            trigger_def: trig_def.clone(),
                            ability,
                            timestamp,
                        });
                    }
                }
            }

            // Keyword-based triggers: Prowess
            // Prowess triggers when the controller casts a noncreature spell.
            // Cards define Prowess as K:Prowess with no explicit trigger_definition,
            // so we synthetically generate the trigger here.
            if let GameEvent::SpellCast {
                card_id,
                controller: caster,
            } = event
            {
                if has_prowess && *caster == controller {
                    // Check if the cast spell is noncreature
                    let is_noncreature = state
                        .objects
                        .iter()
                        .find(|(_, obj)| obj.card_id == *card_id)
                        .map(|(_, obj)| !obj.card_types.core_types.contains(&CoreType::Creature))
                        .unwrap_or(false);

                    if is_noncreature {
                        let prowess_ability = ResolvedAbility {
                            api_type: "Pump".to_string(),
                            params: HashMap::from([
                                ("NumAtt".to_string(), "+1".to_string()),
                                ("NumDef".to_string(), "+1".to_string()),
                            ]),
                            targets: Vec::new(),
                            source_id: obj_id,
                            controller,
                            sub_ability: None,
                            svars: HashMap::new(),
                        };
                        let prowess_trig_def = TriggerDefinition {
                            mode: "SpellCast".to_string(),
                            params: HashMap::from([
                                ("ValidActivatingPlayer".to_string(), "You".to_string()),
                                ("ValidCard".to_string(), "Card".to_string()),
                            ]),
                        };
                        pending.push(PendingTrigger {
                            source_id: obj_id,
                            controller,
                            trigger_def: prowess_trig_def,
                            ability: prowess_ability,
                            timestamp,
                        });
                    }
                }
            }
        }
    }

    if pending.is_empty() {
        return;
    }

    // APNAP ordering: active player's triggers first on stack (resolve last),
    // then non-active player's. Within same controller, order by timestamp.
    pending.sort_by_key(|t| {
        let is_nap = if t.controller == state.active_player {
            0
        } else {
            1
        };
        (is_nap, t.timestamp)
    });

    // Reverse so NAP triggers are placed first (bottom of stack), AP triggers last (top).
    // LIFO means AP triggers resolve last, which is correct per MTG 603.3b.
    pending.reverse();

    let mut events_out = Vec::new();
    for trigger in pending {
        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;

        let entry = StackEntry {
            id: entry_id,
            source_id: trigger.source_id,
            controller: trigger.controller,
            kind: StackEntryKind::TriggeredAbility {
                source_id: trigger.source_id,
                ability: trigger.ability,
            },
        };
        stack::push_to_stack(state, entry, &mut events_out);
    }
}

/// Build a ResolvedAbility from a TriggerDefinition.
fn build_triggered_ability(
    trig_def: &TriggerDefinition,
    source_id: ObjectId,
    controller: PlayerId,
    svars: &HashMap<String, String>,
) -> ResolvedAbility {
    // Check for "Execute" param pointing to an SVar
    let (api_type, params) = if let Some(execute_svar) = trig_def.params.get("Execute") {
        if let Some(svar_value) = svars.get(execute_svar) {
            // Parse the SVar as an ability string
            if let Ok(ability_def) = crate::parser::ability::parse_ability(svar_value) {
                (ability_def.api_type, ability_def.params)
            } else {
                (String::new(), HashMap::new())
            }
        } else {
            (String::new(), HashMap::new())
        }
    } else {
        // No Execute param -- check for inline api_type in trigger params
        let api_type = trig_def.params.get("ApiType").cloned().unwrap_or_default();
        (api_type, trig_def.params.clone())
    };

    ResolvedAbility {
        api_type,
        params,
        targets: Vec::new(),
        source_id,
        controller,
        sub_ability: None,
        svars: svars.clone(),
    }
}

// ---------------------------------------------------------------------------
// Trigger Registry
// ---------------------------------------------------------------------------

/// Build a registry mapping every TriggerMode to its matcher function.
pub fn build_trigger_registry() -> HashMap<TriggerMode, TriggerMatcher> {
    let mut r: HashMap<TriggerMode, TriggerMatcher> = HashMap::new();

    // Core matchers with real logic
    r.insert(TriggerMode::ChangesZone, match_changes_zone);
    r.insert(TriggerMode::ChangesZoneAll, match_changes_zone_all);
    r.insert(TriggerMode::DamageDone, match_damage_done);
    r.insert(TriggerMode::DamageDoneOnce, match_damage_done);
    r.insert(TriggerMode::DamageAll, match_damage_done);
    r.insert(TriggerMode::DamageDealtOnce, match_damage_done);
    r.insert(TriggerMode::DamageDoneOnceByController, match_damage_done);
    r.insert(TriggerMode::SpellCast, match_spell_cast);
    r.insert(TriggerMode::SpellCastOrCopy, match_spell_cast);
    r.insert(TriggerMode::Attacks, match_attacks);
    r.insert(TriggerMode::AttackersDeclared, match_attackers_declared);
    r.insert(
        TriggerMode::AttackersDeclaredOneTarget,
        match_attackers_declared,
    );
    r.insert(TriggerMode::Blocks, match_blocks);
    r.insert(TriggerMode::BlockersDeclared, match_blockers_declared);
    r.insert(TriggerMode::Countered, match_countered);
    r.insert(TriggerMode::CounterAdded, match_counter_added);
    r.insert(TriggerMode::CounterAddedOnce, match_counter_added);
    r.insert(TriggerMode::CounterAddedAll, match_counter_added);
    r.insert(TriggerMode::CounterRemoved, match_counter_removed);
    r.insert(TriggerMode::CounterRemovedOnce, match_counter_removed);
    r.insert(TriggerMode::Taps, match_taps);
    r.insert(TriggerMode::TapAll, match_taps);
    r.insert(TriggerMode::Untaps, match_untaps);
    r.insert(TriggerMode::UntapAll, match_untaps);
    r.insert(TriggerMode::LifeGained, match_life_gained);
    r.insert(TriggerMode::LifeLost, match_life_lost);
    r.insert(TriggerMode::LifeLostAll, match_life_lost);
    r.insert(TriggerMode::Drawn, match_drawn);
    r.insert(TriggerMode::Discarded, match_discarded);
    r.insert(TriggerMode::DiscardedAll, match_discarded);
    r.insert(TriggerMode::Sacrificed, match_sacrificed);
    r.insert(TriggerMode::SacrificedOnce, match_sacrificed);
    r.insert(TriggerMode::Destroyed, match_destroyed);
    r.insert(TriggerMode::TokenCreated, match_token_created);
    r.insert(TriggerMode::TokenCreatedOnce, match_token_created);
    r.insert(TriggerMode::TurnBegin, match_turn_begin);
    r.insert(TriggerMode::Phase, match_phase);
    r.insert(TriggerMode::BecomesTarget, match_becomes_target);
    r.insert(TriggerMode::BecomesTargetOnce, match_becomes_target);
    r.insert(TriggerMode::LandPlayed, match_land_played);
    r.insert(TriggerMode::SpellCopy, match_spell_cast);
    r.insert(TriggerMode::ManaAdded, match_mana_added);

    // Remaining trigger modes: recognized but not yet matched against events.
    // These return false -- triggers simply don't fire until the specific
    // events they need are implemented.
    let unimplemented_modes = [
        TriggerMode::ChangesController,
        TriggerMode::DamagePreventedOnce,
        TriggerMode::ExcessDamage,
        TriggerMode::ExcessDamageAll,
        TriggerMode::AbilityCast,
        TriggerMode::AbilityResolves,
        TriggerMode::AbilityTriggered,
        TriggerMode::SpellAbilityCast,
        TriggerMode::SpellAbilityCopy,
        TriggerMode::AttackerBlocked,
        TriggerMode::AttackerBlockedOnce,
        TriggerMode::AttackerBlockedByCreature,
        TriggerMode::AttackerUnblocked,
        TriggerMode::AttackerUnblockedOnce,
        TriggerMode::CounterPlayerAddedAll,
        TriggerMode::CounterTypeAddedAll,
        TriggerMode::TapsForMana,
        TriggerMode::Milled,
        TriggerMode::MilledOnce,
        TriggerMode::MilledAll,
        TriggerMode::Exiled,
        TriggerMode::Revealed,
        TriggerMode::Shuffled,
        TriggerMode::PayLife,
        TriggerMode::PayCumulativeUpkeep,
        TriggerMode::PayEcho,
        TriggerMode::TurnFaceUp,
        TriggerMode::Transformed,
        TriggerMode::PhaseIn,
        TriggerMode::PhaseOut,
        TriggerMode::PhaseOutAll,
        TriggerMode::NewGame,
        TriggerMode::BecomeMonarch,
        TriggerMode::TakesInitiative,
        TriggerMode::LosesGame,
        TriggerMode::Championed,
        TriggerMode::Exerted,
        TriggerMode::Crewed,
        TriggerMode::Saddled,
        TriggerMode::Cycled,
        TriggerMode::Evolved,
        TriggerMode::Explored,
        TriggerMode::Exploited,
        TriggerMode::Enlisted,
        TriggerMode::ManaExpend,
        TriggerMode::Attached,
        TriggerMode::Unattach,
        TriggerMode::Adapt,
        TriggerMode::Foretell,
        TriggerMode::Investigated,
        TriggerMode::DungeonCompleted,
        TriggerMode::RoomEntered,
        TriggerMode::PlanarDice,
        TriggerMode::PlaneswalkedFrom,
        TriggerMode::PlaneswalkedTo,
        TriggerMode::ChaosEnsues,
        TriggerMode::RolledDie,
        TriggerMode::RolledDieOnce,
        TriggerMode::FlippedCoin,
        TriggerMode::Clashed,
        TriggerMode::DayTimeChanges,
        TriggerMode::ClassLevelGained,
        TriggerMode::Copied,
        TriggerMode::ConjureAll,
        TriggerMode::Vote,
        TriggerMode::BecomeRenowned,
        TriggerMode::BecomeMonstrous,
        TriggerMode::Proliferate,
        TriggerMode::RingTemptsYou,
        TriggerMode::Surveil,
        TriggerMode::Scry,
        TriggerMode::Fight,
        TriggerMode::FightOnce,
        TriggerMode::Abandoned,
        TriggerMode::CaseSolved,
        TriggerMode::ClaimPrize,
        TriggerMode::CollectEvidence,
        TriggerMode::CommitCrime,
        TriggerMode::CrankContraption,
        TriggerMode::Devoured,
        TriggerMode::Discover,
        TriggerMode::Forage,
        TriggerMode::FullyUnlock,
        TriggerMode::GiveGift,
        TriggerMode::ManifestDread,
        TriggerMode::Mentored,
        TriggerMode::Mutates,
        TriggerMode::SearchedLibrary,
        TriggerMode::SeekAll,
        TriggerMode::SetInMotion,
        TriggerMode::Specializes,
        TriggerMode::Stationed,
        TriggerMode::Trains,
        TriggerMode::UnlockDoor,
        TriggerMode::VisitAttraction,
        TriggerMode::BecomesCrewed,
        TriggerMode::BecomesPlotted,
        TriggerMode::BecomesSaddled,
        TriggerMode::Immediate,
        TriggerMode::Always,
        TriggerMode::Airbend,
        TriggerMode::Earthbend,
        TriggerMode::Firebend,
        TriggerMode::Waterbend,
        TriggerMode::ElementalBend,
    ];

    for mode in unimplemented_modes {
        // TODO: implement when relevant cards need it
        r.insert(mode, match_unimplemented);
    }

    r
}

// ---------------------------------------------------------------------------
// Core Trigger Matchers (~20 with real logic)
// ---------------------------------------------------------------------------

fn match_changes_zone(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::ZoneChanged {
        object_id,
        from,
        to,
    } = event
    {
        if let Some(origin) = params.get("Origin") {
            if origin != "Any" && !zone_matches(origin, from) {
                return false;
            }
        }
        if let Some(dest) = params.get("Destination") {
            if dest != "Any" && !zone_matches(dest, to) {
                return false;
            }
        }
        if let Some(filter) = params.get("ValidCard") {
            if !card_matches_filter(state, *object_id, filter, source_id) {
                return false;
            }
        }
        true
    } else {
        false
    }
}

fn match_changes_zone_all(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // ChangesZoneAll triggers for any card changing zones, same logic
    match_changes_zone(event, params, source_id, state)
}

fn match_damage_done(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::DamageDealt {
        source_id: dmg_source,
        target: _,
        amount,
    } = event
    {
        // Check if trigger requires damage from a specific source
        if let Some(filter) = params.get("ValidSource") {
            if !card_matches_filter(state, *dmg_source, filter, source_id) {
                return false;
            }
        }
        // Check minimum damage amount
        if let Some(min) = params.get("DamageAmount") {
            if let Ok(min_val) = min.parse::<u32>() {
                if *amount < min_val {
                    return false;
                }
            }
        }
        true
    } else {
        false
    }
}

fn match_spell_cast(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::SpellCast {
        card_id,
        controller,
    } = event
    {
        // Check ValidCard filter
        if let Some(filter) = params.get("ValidCard") {
            // Find object by card_id
            let obj_id = state
                .objects
                .iter()
                .find(|(_, obj)| obj.card_id == *card_id)
                .map(|(id, _)| *id);
            if let Some(oid) = obj_id {
                if !card_matches_filter(state, oid, filter, source_id) {
                    return false;
                }
            }
        }
        // Check controller filter
        if let Some(caster) = params.get("ValidActivatingPlayer") {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match caster.as_str() {
                "You" => {
                    if trigger_controller != Some(*controller) {
                        return false;
                    }
                }
                "Opponent" => {
                    if trigger_controller == Some(*controller) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    } else {
        false
    }
}

fn match_attacks(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::AttackersDeclared { attacker_ids, .. } = event {
        // "Attacks" triggers for the specific source creature attacking
        if let Some(filter) = params.get("ValidCard") {
            attacker_ids
                .iter()
                .any(|id| card_matches_filter(state, *id, filter, source_id))
        } else {
            // No filter: trigger if source itself is among attackers
            attacker_ids.contains(&source_id)
        }
    } else {
        false
    }
}

fn match_attackers_declared(
    event: &GameEvent,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::AttackersDeclared { .. })
}

fn match_blocks(
    event: &GameEvent,
    _params: &HashMap<String, String>,
    source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::BlockersDeclared { assignments } = event {
        // Blocks trigger: source creature is among blockers
        assignments.iter().any(|(blocker, _)| *blocker == source_id)
    } else {
        false
    }
}

fn match_blockers_declared(
    event: &GameEvent,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::BlockersDeclared { .. })
}

fn match_countered(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::SpellCountered { object_id, .. } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            true
        }
    } else {
        false
    }
}

fn match_counter_added(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CounterAdded {
        object_id,
        counter_type,
        ..
    } = event
    {
        if let Some(ct) = params.get("CounterType") {
            if ct != counter_type {
                return false;
            }
        }
        if let Some(filter) = params.get("ValidCard") {
            if !card_matches_filter(state, *object_id, filter, source_id) {
                return false;
            }
        }
        true
    } else {
        false
    }
}

fn match_counter_removed(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CounterRemoved {
        object_id,
        counter_type,
        ..
    } = event
    {
        if let Some(ct) = params.get("CounterType") {
            if ct != counter_type {
                return false;
            }
        }
        if let Some(filter) = params.get("ValidCard") {
            if !card_matches_filter(state, *object_id, filter, source_id) {
                return false;
            }
        }
        true
    } else {
        false
    }
}

fn match_taps(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PermanentTapped { object_id } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            *object_id == source_id
        }
    } else {
        false
    }
}

fn match_untaps(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PermanentUntapped { object_id } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            *object_id == source_id
        }
    } else {
        false
    }
}

fn match_life_gained(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::LifeChanged { player_id, amount } = event {
        if *amount <= 0 {
            return false;
        }
        if let Some(filter) = params.get("ValidPlayer") {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match filter.as_str() {
                "You" => trigger_controller == Some(*player_id),
                "Opponent" => trigger_controller != Some(*player_id),
                _ => true,
            }
        } else {
            true
        }
    } else {
        false
    }
}

fn match_life_lost(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::LifeChanged { player_id, amount } = event {
        if *amount >= 0 {
            return false;
        }
        if let Some(filter) = params.get("ValidPlayer") {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match filter.as_str() {
                "You" => trigger_controller == Some(*player_id),
                "Opponent" => trigger_controller != Some(*player_id),
                _ => true,
            }
        } else {
            true
        }
    } else {
        false
    }
}

fn match_drawn(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CardDrawn { player_id, .. } = event {
        if let Some(filter) = params.get("ValidPlayer") {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match filter.as_str() {
                "You" => trigger_controller == Some(*player_id),
                "Opponent" => trigger_controller != Some(*player_id),
                _ => true,
            }
        } else {
            true
        }
    } else {
        false
    }
}

fn match_discarded(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::Discarded {
        player_id,
        object_id,
    } = event
    {
        if let Some(filter) = params.get("ValidCard") {
            if !card_matches_filter(state, *object_id, filter, source_id) {
                return false;
            }
        }
        if let Some(player_filter) = params.get("ValidPlayer") {
            let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
            match player_filter.as_str() {
                "You" => return trigger_controller == Some(*player_id),
                "Opponent" => return trigger_controller != Some(*player_id),
                _ => {}
            }
        }
        true
    } else {
        false
    }
}

fn match_sacrificed(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::PermanentSacrificed { object_id, .. } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            true
        }
    } else {
        false
    }
}

fn match_destroyed(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::CreatureDestroyed { object_id } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            true
        }
    } else {
        false
    }
}

fn match_token_created(
    event: &GameEvent,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::TokenCreated { name, .. } = event {
        if let Some(token_name) = params.get("ValidToken") {
            if token_name != "Any" && token_name != name {
                return false;
            }
        }
        true
    } else {
        false
    }
}

fn match_turn_begin(
    event: &GameEvent,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::TurnStarted { .. })
}

fn match_phase(
    event: &GameEvent,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    if let GameEvent::PhaseChanged { phase } = event {
        if let Some(phase_name) = params.get("Phase") {
            let phase_str = format!("{:?}", phase);
            phase_str == *phase_name
        } else {
            true
        }
    } else {
        false
    }
}

fn match_becomes_target(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::BecomesTarget { object_id, .. } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            *object_id == source_id
        }
    } else {
        false
    }
}

fn match_land_played(
    event: &GameEvent,
    params: &HashMap<String, String>,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    if let GameEvent::LandPlayed { object_id, .. } = event {
        if let Some(filter) = params.get("ValidCard") {
            card_matches_filter(state, *object_id, filter, source_id)
        } else {
            true
        }
    } else {
        false
    }
}

fn match_mana_added(
    event: &GameEvent,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, GameEvent::ManaAdded { .. })
}

/// Fallback matcher for unimplemented trigger modes. Always returns false.
fn match_unimplemented(
    _event: &GameEvent,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
    _state: &GameState,
) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Helper Functions
// ---------------------------------------------------------------------------

/// Check if a zone param string matches an actual Zone value.
fn zone_matches(param: &str, zone: &Zone) -> bool {
    match param {
        "Any" => true,
        "Battlefield" => *zone == Zone::Battlefield,
        "Hand" => *zone == Zone::Hand,
        "Graveyard" => *zone == Zone::Graveyard,
        "Library" => *zone == Zone::Library,
        "Stack" => *zone == Zone::Stack,
        "Exile" => *zone == Zone::Exile,
        "Command" => *zone == Zone::Command,
        _ => false,
    }
}

/// Basic card filter matching for ValidCard params.
/// Parse Forge-style dot-separated qualifiers.
fn card_matches_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &str,
    source_id: ObjectId,
) -> bool {
    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    for part in filter.split('.') {
        match part {
            // Type checks
            "Creature" => {
                if !obj.card_types.core_types.contains(&CoreType::Creature) {
                    return false;
                }
            }
            "Land" => {
                if !obj.card_types.core_types.contains(&CoreType::Land) {
                    return false;
                }
            }
            "Artifact" => {
                if !obj.card_types.core_types.contains(&CoreType::Artifact) {
                    return false;
                }
            }
            "Enchantment" => {
                if !obj.card_types.core_types.contains(&CoreType::Enchantment) {
                    return false;
                }
            }
            "Instant" => {
                if !obj.card_types.core_types.contains(&CoreType::Instant) {
                    return false;
                }
            }
            "Sorcery" => {
                if !obj.card_types.core_types.contains(&CoreType::Sorcery) {
                    return false;
                }
            }
            "Planeswalker" => {
                if !obj.card_types.core_types.contains(&CoreType::Planeswalker) {
                    return false;
                }
            }
            "Card" | "Permanent" | "Any" => {
                // Matches anything
            }
            // Controller checks
            "YouCtrl" => {
                let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
                if trigger_controller != Some(obj.controller) {
                    return false;
                }
            }
            "OppCtrl" => {
                let trigger_controller = state.objects.get(&source_id).map(|o| o.controller);
                if trigger_controller == Some(obj.controller) {
                    return false;
                }
            }
            // Self-reference
            "Self" => {
                if object_id != source_id {
                    return false;
                }
            }
            // Other (unrecognized) -- permissive fallback
            _ => {}
        }
    }

    true
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::game::zones::create_object;
    use crate::types::ability::TriggerDefinition;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn changes_zone_etb_matches() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("Origin".to_string(), "Any".to_string());
        params.insert("Destination".to_string(), "Battlefield".to_string());

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Hand,
            to: Zone::Battlefield,
        };
        assert!(match_changes_zone(&event, &params, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_dies_matches() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("Origin".to_string(), "Battlefield".to_string());
        params.insert("Destination".to_string(), "Graveyard".to_string());

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        };
        assert!(match_changes_zone(&event, &params, ObjectId(1), &state));
    }

    #[test]
    fn changes_zone_wrong_destination_no_match() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("Destination".to_string(), "Battlefield".to_string());

        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Hand,
            to: Zone::Graveyard,
        };
        assert!(!match_changes_zone(&event, &params, ObjectId(1), &state));
    }

    #[test]
    fn damage_done_matches() {
        let state = setup();
        let params = HashMap::new();

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: crate::types::ability::TargetRef::Player(PlayerId(0)),
            amount: 3,
        };
        assert!(match_damage_done(&event, &params, ObjectId(1), &state));
    }

    #[test]
    fn damage_done_amount_threshold() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("DamageAmount".to_string(), "5".to_string());

        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: crate::types::ability::TargetRef::Player(PlayerId(0)),
            amount: 3,
        };
        assert!(!match_damage_done(&event, &params, ObjectId(1), &state));

        let event_high = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: crate::types::ability::TargetRef::Player(PlayerId(0)),
            amount: 5,
        };
        assert!(match_damage_done(&event_high, &params, ObjectId(1), &state));
    }

    #[test]
    fn spell_cast_matches() {
        let state = setup();
        let params = HashMap::new();

        let event = GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(0),
        };
        assert!(match_spell_cast(&event, &params, ObjectId(1), &state));
    }

    #[test]
    fn unknown_trigger_mode_doesnt_crash() {
        let registry = build_trigger_registry();
        let unknown = TriggerMode::Unknown("FakeMode".to_string());
        // Unknown modes are not in the registry
        assert!(registry.get(&unknown).is_none());
    }

    #[test]
    fn registry_has_all_137_modes() {
        let registry = build_trigger_registry();
        // Count all registered modes (should be 137+)
        assert!(
            registry.len() >= 137,
            "Expected 137+ registered trigger modes, got {}",
            registry.len()
        );
    }

    #[test]
    fn apnap_ordering() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create two creatures with triggers on battlefield
        let p0_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&p0_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: "ChangesZone".to_string(),
                params: HashMap::from([
                    ("Origin".to_string(), "Any".to_string()),
                    ("Destination".to_string(), "Battlefield".to_string()),
                    ("Execute".to_string(), "TrigAbility".to_string()),
                ]),
            });
            obj.svars.insert(
                "TrigAbility".to_string(),
                "DB$ Draw | NumCards$ 1".to_string(),
            );
        }

        let p1_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&p1_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.controller = PlayerId(1);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: "ChangesZone".to_string(),
                params: HashMap::from([
                    ("Origin".to_string(), "Any".to_string()),
                    ("Destination".to_string(), "Battlefield".to_string()),
                    ("Execute".to_string(), "TrigAbility".to_string()),
                ]),
            });
            obj.svars.insert(
                "TrigAbility".to_string(),
                "DB$ Draw | NumCards$ 1".to_string(),
            );
        }

        // Trigger event
        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Both triggers should be on the stack
        assert_eq!(state.stack.len(), 2);

        // AP (P0) triggers should be on top of stack (resolve last = placed last)
        // NAP (P1) triggers should be on bottom (resolve first = placed first)
        let top = &state.stack[state.stack.len() - 1];
        let bottom = &state.stack[0];
        assert_eq!(top.controller, PlayerId(0), "AP trigger should be on top");
        assert_eq!(
            bottom.controller,
            PlayerId(1),
            "NAP trigger should be on bottom"
        );
    }

    #[test]
    fn zone_matches_helper() {
        assert!(zone_matches("Battlefield", &Zone::Battlefield));
        assert!(zone_matches("Hand", &Zone::Hand));
        assert!(zone_matches("Graveyard", &Zone::Graveyard));
        assert!(zone_matches("Any", &Zone::Exile));
        assert!(!zone_matches("Battlefield", &Zone::Graveyard));
    }

    #[test]
    fn card_matches_filter_creature() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(card_matches_filter(&state, id, "Creature", ObjectId(99)));
        assert!(!card_matches_filter(&state, id, "Land", ObjectId(99)));
        assert!(card_matches_filter(&state, id, "Card", ObjectId(99)));
        assert!(card_matches_filter(&state, id, "Any", ObjectId(99)));
    }

    #[test]
    fn card_matches_filter_you_ctrl() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let opp_target = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(card_matches_filter(
            &state,
            target,
            "Creature.YouCtrl",
            source
        ));
        assert!(!card_matches_filter(
            &state,
            opp_target,
            "Creature.YouCtrl",
            source
        ));
    }

    #[test]
    fn card_matches_filter_self() {
        let mut state = setup();
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        assert!(card_matches_filter(&state, obj, "Card.Self", obj));
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );
        assert!(!card_matches_filter(&state, obj, "Card.Self", other));
    }

    #[test]
    fn life_gained_matches_positive() {
        let state = setup();
        let params = HashMap::new();
        let event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        };
        assert!(match_life_gained(&event, &params, ObjectId(1), &state));

        let loss_event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: -3,
        };
        assert!(!match_life_gained(
            &loss_event,
            &params,
            ObjectId(1),
            &state
        ));
    }

    #[test]
    fn life_lost_matches_negative() {
        let state = setup();
        let params = HashMap::new();
        let event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: -3,
        };
        assert!(match_life_lost(&event, &params, ObjectId(1), &state));

        let gain_event = GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        };
        assert!(!match_life_lost(&gain_event, &params, ObjectId(1), &state));
    }

    // === Integration tests for engine trigger processing ===

    #[test]
    fn etb_trigger_places_ability_on_stack() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a permanent with an ETB trigger on battlefield
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "ETB Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: "ChangesZone".to_string(),
                params: HashMap::from([
                    ("Origin".to_string(), "Any".to_string()),
                    ("Destination".to_string(), "Battlefield".to_string()),
                    ("Execute".to_string(), "TrigAbility".to_string()),
                ]),
            });
            obj.svars.insert(
                "TrigAbility".to_string(),
                "DB$ Draw | NumCards$ 1".to_string(),
            );
        }

        // Simulate a ZoneChanged event (another creature enters)
        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        // Trigger should be on the stack
        assert_eq!(state.stack.len(), 1);
        let entry = &state.stack[0];
        assert_eq!(entry.source_id, trigger_creature);
        assert_eq!(entry.controller, PlayerId(0));
        match &entry.kind {
            StackEntryKind::TriggeredAbility { source_id, ability } => {
                assert_eq!(*source_id, trigger_creature);
                assert_eq!(ability.api_type, "Draw");
                assert_eq!(ability.params.get("NumCards").unwrap(), "1");
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }
    }

    #[test]
    fn multiple_triggers_from_same_event() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create two creatures with ETB triggers, different controllers
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 ETB".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c1).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: "ChangesZone".to_string(),
                params: HashMap::from([
                    ("Origin".to_string(), "Any".to_string()),
                    ("Destination".to_string(), "Battlefield".to_string()),
                    ("Execute".to_string(), "TrigAbility".to_string()),
                ]),
            });
            obj.svars.insert(
                "TrigAbility".to_string(),
                "DB$ Draw | NumCards$ 1".to_string(),
            );
        }

        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 ETB".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c2).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.controller = PlayerId(1);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: "ChangesZone".to_string(),
                params: HashMap::from([
                    ("Origin".to_string(), "Any".to_string()),
                    ("Destination".to_string(), "Battlefield".to_string()),
                    ("Execute".to_string(), "TrigAbility".to_string()),
                ]),
            });
            obj.svars.insert(
                "TrigAbility".to_string(),
                "DB$ Draw | NumCards$ 1".to_string(),
            );
        }

        let events = vec![GameEvent::ZoneChanged {
            object_id: ObjectId(99),
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];

        process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 2);
        // APNAP: AP (P0) on top, NAP (P1) on bottom
        assert_eq!(state.stack[state.stack.len() - 1].controller, PlayerId(0));
        assert_eq!(state.stack[0].controller, PlayerId(1));
    }

    #[test]
    fn trigger_with_condition_only_matches_when_met() {
        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a trigger that only fires for creature zone changes
        let trigger_src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Trigger Source".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_src).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: "ChangesZone".to_string(),
                params: HashMap::from([
                    ("Origin".to_string(), "Any".to_string()),
                    ("Destination".to_string(), "Battlefield".to_string()),
                    ("ValidCard".to_string(), "Creature".to_string()),
                    ("Execute".to_string(), "TrigAbility".to_string()),
                ]),
            });
            obj.svars.insert(
                "TrigAbility".to_string(),
                "DB$ Draw | NumCards$ 1".to_string(),
            );
        }

        // Create a non-creature that enters
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Land enters -- should NOT trigger (ValidCard = Creature)
        let events = vec![GameEvent::ZoneChanged {
            object_id: land,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];
        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            0,
            "Land entering should not trigger creature-only ETB"
        );

        // Now a creature enters -- should trigger
        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let events = vec![GameEvent::ZoneChanged {
            object_id: creature,
            from: Zone::Hand,
            to: Zone::Battlefield,
        }];
        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Creature entering should trigger creature ETB"
        );
    }

    #[test]
    fn prowess_triggers_on_noncreature_spell_cast() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        state.active_player = PlayerId(0);

        // Create a creature with Prowess keyword on the battlefield
        let prowess_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monastery Swiftspear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prowess_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Prowess);
        }

        // Create a noncreature spell object (Instant) on stack for the SpellCast event
        let spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        // Simulate SpellCast event by controller
        let events = vec![GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(0),
        }];

        process_triggers(&mut state, &events);

        // Prowess should have placed a triggered ability on the stack
        assert_eq!(
            state.stack.len(),
            1,
            "Prowess should trigger on noncreature spell"
        );
    }

    #[test]
    fn prowess_does_not_trigger_on_creature_spell() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        state.active_player = PlayerId(0);

        let prowess_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monastery Swiftspear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prowess_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Prowess);
        }

        // Create a creature spell
        let creature_spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bear Cub".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&creature_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let events = vec![GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(0),
        }];

        process_triggers(&mut state, &events);

        // Prowess should NOT trigger on creature spells
        assert_eq!(
            state.stack.len(),
            0,
            "Prowess should not trigger on creature spell"
        );
    }

    #[test]
    fn prowess_does_not_trigger_on_opponent_spell() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        state.active_player = PlayerId(0);

        let prowess_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monastery Swiftspear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prowess_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            obj.keywords.push(Keyword::Prowess);
        }

        // Opponent casts a noncreature spell
        let spell = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let events = vec![GameEvent::SpellCast {
            card_id: CardId(10),
            controller: PlayerId(1),
        }];

        process_triggers(&mut state, &events);

        // Prowess should NOT trigger on opponent's spells
        assert_eq!(
            state.stack.len(),
            0,
            "Prowess should not trigger on opponent's spell"
        );
    }
}

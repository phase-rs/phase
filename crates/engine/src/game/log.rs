use crate::types::ability::TargetRef;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::log::{GameLogEntry, LogCategory, LogSegment};
use crate::types::player::PlayerId;

/// Resolve a batch of events into structured log entries.
/// Events that would leak hidden information (e.g., cards drawn from library) are filtered out.
pub fn resolve_log_entries(events: &[GameEvent], state: &GameState) -> Vec<GameLogEntry> {
    events
        .iter()
        .filter(|event| !should_exclude_event(event, state))
        .map(|event| GameLogEntry {
            seq: 0, // Assigned by frontend
            turn: state.turn_number,
            phase: state.phase,
            category: categorize(event),
            segments: format_segments(event, state),
        })
        .collect()
}

/// Returns true for events that should be excluded from log output.
/// Covers hidden-information leaks and low-signal stack bookkeeping.
fn should_exclude_event(event: &GameEvent, _state: &GameState) -> bool {
    match event {
        // Individual card draws from library leak card identity — CardsDrawn summary suffices
        GameEvent::ZoneChanged {
            from: Some(crate::types::zones::Zone::Library),
            ..
        } => true,
        // CardDrawn also reveals which specific card was drawn
        GameEvent::CardDrawn { .. } => true,
        // StackPushed/StackResolved are low-signal bookkeeping —
        // the meaningful info is in SpellCast/AbilityActivated and EffectResolved
        GameEvent::StackPushed { .. } | GameEvent::StackResolved { .. } => true,
        _ => false,
    }
}

/// Resolve an object's display name from state, falling back to LKI cache.
fn resolve_object_name(state: &GameState, id: ObjectId) -> String {
    if let Some(obj) = state.objects.get(&id) {
        return obj.name.clone();
    }
    if let Some(lki) = state.lki_cache.get(&id) {
        return lki.name.clone();
    }
    format!("(unknown #{})", id.0)
}

/// Resolve a player's display name from `log_player_names` or default to "Player N".
fn resolve_player_name(state: &GameState, id: PlayerId) -> String {
    state
        .log_player_names
        .get(id.0 as usize)
        .filter(|n| !n.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("Player {}", id.0 + 1))
}

fn card_seg(state: &GameState, id: ObjectId) -> LogSegment {
    LogSegment::CardName {
        name: resolve_object_name(state, id),
        object_id: id,
    }
}

fn player_seg(state: &GameState, id: PlayerId) -> LogSegment {
    LogSegment::PlayerName {
        name: resolve_player_name(state, id),
        player_id: id,
    }
}

fn text(s: &str) -> LogSegment {
    LogSegment::Text(s.to_string())
}

fn num(n: i32) -> LogSegment {
    LogSegment::Number(n)
}

/// Exhaustive categorization of game events.
fn categorize(event: &GameEvent) -> LogCategory {
    match event {
        GameEvent::GameStarted
        | GameEvent::GameOver { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::PlayerEliminated { .. }
        | GameEvent::MulliganStarted => LogCategory::Game,

        GameEvent::TurnStarted { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PriorityPassed { .. } => LogCategory::Turn,

        GameEvent::SpellCast { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::StackResolved { .. }
        | GameEvent::SpellCountered { .. } => LogCategory::Stack,

        GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. } => LogCategory::Combat,

        GameEvent::DamageDealt { is_combat, .. } => {
            if *is_combat {
                LogCategory::Combat
            } else {
                LogCategory::Life
            }
        }

        GameEvent::DamagePrevented { .. } => LogCategory::Life,

        GameEvent::ZoneChanged { .. }
        | GameEvent::LandPlayed { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::CardsDrawn { .. }
        | GameEvent::Discarded { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::CardsRevealed { .. } => LogCategory::Zone,

        GameEvent::LifeChanged { .. } => LogCategory::Life,

        GameEvent::ManaAdded { .. } => LogCategory::Mana,

        GameEvent::PermanentTapped { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. } => LogCategory::State,

        GameEvent::SpeedChanged { .. } => LogCategory::Special,

        GameEvent::TokenCreated { .. } | GameEvent::ObjectConjured { .. } => LogCategory::Token,

        GameEvent::EffectResolved { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::CascadeMissed { .. } => LogCategory::Trigger,

        GameEvent::CreatureDestroyed { .. } | GameEvent::PermanentSacrificed { .. } => {
            LogCategory::Destroy
        }

        GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. }
        | GameEvent::CompanionRevealed { .. }
        | GameEvent::CompanionMovedToHand { .. }
        | GameEvent::EnergyChanged { .. }
        | GameEvent::PlayerCounterChanged { .. }
        | GameEvent::ManaExpended { .. }
        | GameEvent::PlayerPerformedAction { .. }
        | GameEvent::RoomEntered { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. }
        | GameEvent::XValueChosen { .. } => LogCategory::Special,
        GameEvent::CombatTaxPaid { .. } | GameEvent::CombatTaxDeclined { .. } => {
            LogCategory::Combat
        }
    }
}

/// Exhaustive segment formatting for all event variants.
fn format_segments(event: &GameEvent, state: &GameState) -> Vec<LogSegment> {
    match event {
        GameEvent::GameStarted => vec![text("Game started")],

        GameEvent::TurnStarted {
            player_id,
            turn_number,
        } => vec![
            text("Turn "),
            num(*turn_number as i32),
            text(" — "),
            player_seg(state, *player_id),
        ],

        GameEvent::PhaseChanged { phase } => {
            vec![text("Phase: "), text(&format!("{phase:?}"))]
        }

        GameEvent::PriorityPassed { player_id } => {
            vec![player_seg(state, *player_id), text(" passes priority")]
        }

        GameEvent::PlayerPerformedAction { player_id, action } => vec![
            player_seg(state, *player_id),
            text(" performed action "),
            text(&format!("{action:?}")),
        ],

        GameEvent::SpellCast {
            controller,
            object_id,
            ..
        } => vec![
            player_seg(state, *controller),
            text(" casts "),
            card_seg(state, *object_id),
        ],

        GameEvent::AbilityActivated { source_id } => {
            vec![text("Ability activated: "), card_seg(state, *source_id)]
        }

        GameEvent::NinjutsuActivated {
            player_id,
            source_id,
        } => vec![
            player_seg(state, *player_id),
            text(" activates ninjutsu: "),
            card_seg(state, *source_id),
        ],

        GameEvent::StackPushed { object_id } => {
            vec![card_seg(state, *object_id), text(" added to stack")]
        }

        GameEvent::StackResolved { object_id } => {
            vec![card_seg(state, *object_id), text(" resolves")]
        }

        GameEvent::SpellCountered {
            object_id,
            countered_by,
        } => vec![
            card_seg(state, *countered_by),
            text(" counters "),
            card_seg(state, *object_id),
        ],

        // CR 111.1 + CR 603.6a: `from: None` indicates token creation (no prior
        // zone). Render without a source zone to avoid "moves from None to
        // Battlefield" — the `TokenCreated` event carries the created-token
        // name/controller for richer logging.
        GameEvent::ZoneChanged {
            object_id,
            from: Some(from),
            to,
            ..
        } => vec![
            card_seg(state, *object_id),
            text(" moves from "),
            LogSegment::Zone(*from),
            text(" to "),
            LogSegment::Zone(*to),
        ],
        GameEvent::ZoneChanged {
            object_id,
            from: None,
            to,
            ..
        } => vec![
            card_seg(state, *object_id),
            text(" enters "),
            LogSegment::Zone(*to),
        ],

        GameEvent::LandPlayed {
            object_id,
            player_id,
        } => vec![
            player_seg(state, *player_id),
            text(" plays "),
            card_seg(state, *object_id),
        ],

        GameEvent::CardDrawn { player_id, .. } => {
            vec![player_seg(state, *player_id), text(" draws a card")]
        }

        GameEvent::CardsDrawn { player_id, count } => vec![
            player_seg(state, *player_id),
            text(" draws "),
            num(*count as i32),
            text(" cards"),
        ],

        GameEvent::Discarded {
            player_id,
            object_id,
        } => vec![
            player_seg(state, *player_id),
            text(" discards "),
            card_seg(state, *object_id),
        ],

        GameEvent::Cycled {
            player_id,
            object_id,
        } => vec![
            player_seg(state, *player_id),
            text(" cycles "),
            card_seg(state, *object_id),
        ],

        GameEvent::CardsRevealed {
            player, card_names, ..
        } => vec![
            player_seg(state, *player),
            text(" reveals: "),
            text(&card_names.join(", ")),
        ],

        GameEvent::LifeChanged { player_id, amount } => {
            if *amount >= 0 {
                vec![
                    player_seg(state, *player_id),
                    text(" gains "),
                    num(*amount),
                    text(" life"),
                ]
            } else {
                vec![
                    player_seg(state, *player_id),
                    text(" loses "),
                    num(amount.abs()),
                    text(" life"),
                ]
            }
        }

        GameEvent::SpeedChanged {
            player,
            old_speed,
            new_speed,
        } => {
            let old_speed = i32::from(old_speed.unwrap_or(0));
            let new_speed = i32::from(new_speed.unwrap_or(0));
            vec![
                player_seg(state, *player),
                text(" speed changes from "),
                num(old_speed),
                text(" to "),
                num(new_speed),
            ]
        }

        GameEvent::DamageDealt {
            source_id,
            target,
            amount,
            is_combat,
            ..
        } => {
            let combat_text = if *is_combat {
                " combat damage to "
            } else {
                " damage to "
            };
            let target_seg = match target {
                TargetRef::Player(pid) => player_seg(state, *pid),
                TargetRef::Object(oid) => card_seg(state, *oid),
            };
            vec![
                card_seg(state, *source_id),
                text(" deals "),
                num(*amount as i32),
                text(combat_text),
                target_seg,
            ]
        }

        GameEvent::DamagePrevented {
            source_id,
            target,
            amount,
        } => {
            let target_seg = match target {
                TargetRef::Player(pid) => player_seg(state, *pid),
                TargetRef::Object(oid) => card_seg(state, *oid),
            };
            vec![
                num(*amount as i32),
                text(" damage to "),
                target_seg,
                text(" from "),
                card_seg(state, *source_id),
                text(" prevented"),
            ]
        }

        GameEvent::AttackersDeclared {
            attacker_ids,
            defending_player,
            ..
        } => {
            let mut segs = vec![
                player_seg(state, *defending_player),
                text(" is attacked by "),
            ];
            for (i, id) in attacker_ids.iter().enumerate() {
                if i > 0 {
                    segs.push(text(", "));
                }
                segs.push(card_seg(state, *id));
            }
            segs
        }

        GameEvent::BlockersDeclared { assignments } => {
            if assignments.is_empty() {
                return vec![text("No blockers declared")];
            }
            let mut segs = Vec::new();
            for (i, (blocker, attacker)) in assignments.iter().enumerate() {
                if i > 0 {
                    segs.push(text("; "));
                }
                segs.push(card_seg(state, *blocker));
                segs.push(text(" blocks "));
                segs.push(card_seg(state, *attacker));
            }
            segs
        }

        GameEvent::CombatDamageDealtToPlayer {
            player_id,
            source_ids,
        } => vec![
            player_seg(state, *player_id),
            text(" is dealt combat damage by "),
            num(source_ids.len() as i32),
            text(" creature(s)"),
        ],

        GameEvent::ManaAdded {
            source_id,
            mana_type,
            ..
        } => vec![
            card_seg(state, *source_id),
            text(" adds "),
            LogSegment::Mana(format!("{mana_type:?}")),
            text(" mana"),
        ],

        GameEvent::PermanentTapped { object_id, .. } => {
            vec![card_seg(state, *object_id), text(" tapped")]
        }

        GameEvent::PermanentUntapped { object_id } => {
            vec![card_seg(state, *object_id), text(" untapped")]
        }

        GameEvent::PermanentPhasedOut {
            object_id,
            indirect,
        } => {
            if *indirect {
                vec![card_seg(state, *object_id), text(" phased out (indirect)")]
            } else {
                vec![card_seg(state, *object_id), text(" phased out")]
            }
        }

        GameEvent::PermanentPhasedIn { object_id } => {
            vec![card_seg(state, *object_id), text(" phased in")]
        }

        GameEvent::PlayerPhasedOut { player_id } => {
            vec![player_seg(state, *player_id), text(" phased out")]
        }

        GameEvent::PlayerPhasedIn { player_id } => {
            vec![player_seg(state, *player_id), text(" phased in")]
        }

        GameEvent::DamageCleared { object_id } => {
            vec![text("Damage cleared from "), card_seg(state, *object_id)]
        }

        GameEvent::CounterAdded {
            object_id,
            counter_type,
            count,
        } => vec![
            num(*count as i32),
            text(" "),
            LogSegment::Keyword(format!("{counter_type:?}")),
            text(" counter(s) on "),
            card_seg(state, *object_id),
        ],

        GameEvent::CounterRemoved {
            object_id,
            counter_type,
            count,
        } => vec![
            num(*count as i32),
            text(" "),
            LogSegment::Keyword(format!("{counter_type:?}")),
            text(" counter(s) removed from "),
            card_seg(state, *object_id),
        ],

        GameEvent::Transformed { object_id } => {
            vec![card_seg(state, *object_id), text(" transforms")]
        }

        GameEvent::TurnedFaceUp { object_id } => {
            vec![card_seg(state, *object_id), text(" is turned face up")]
        }

        GameEvent::Regenerated { object_id } => {
            vec![card_seg(state, *object_id), text(" regenerates")]
        }

        GameEvent::CreatureSuspected { object_id } => {
            vec![card_seg(state, *object_id), text(" becomes suspected")]
        }

        GameEvent::BecamePrepared { object_id } => {
            vec![card_seg(state, *object_id), text(" becomes prepared")]
        }

        GameEvent::BecameUnprepared { object_id } => {
            vec![card_seg(state, *object_id), text(" becomes unprepared")]
        }

        GameEvent::CaseSolved { object_id } => {
            vec![card_seg(state, *object_id), text(" is solved")]
        }

        GameEvent::ClassLevelGained { object_id, level } => vec![
            card_seg(state, *object_id),
            text(" gains level "),
            num(*level as i32),
        ],

        GameEvent::DayNightChanged { new_state } => {
            vec![text("Day/Night changed to "), text(new_state)]
        }

        GameEvent::TokenCreated { object_id, name } => vec![
            text("Token created: "),
            LogSegment::CardName {
                name: name.clone(),
                object_id: *object_id,
            },
        ],

        GameEvent::ObjectConjured { object_id, name } => vec![
            text("Conjured: "),
            LogSegment::CardName {
                name: name.clone(),
                object_id: *object_id,
            },
        ],

        GameEvent::CreatureDestroyed { object_id } => {
            vec![card_seg(state, *object_id), text(" is destroyed")]
        }

        GameEvent::PermanentSacrificed {
            object_id,
            player_id,
        } => vec![
            player_seg(state, *player_id),
            text(" sacrifices "),
            card_seg(state, *object_id),
        ],

        GameEvent::EffectResolved { kind, source_id } => vec![
            card_seg(state, *source_id),
            text(": "),
            text(&format!("{kind:?}")),
        ],

        GameEvent::BecomesTarget {
            object_id,
            source_id,
        } => vec![
            card_seg(state, *object_id),
            text(" is targeted by "),
            card_seg(state, *source_id),
        ],

        GameEvent::ReplacementApplied {
            source_id,
            event_type,
        } => vec![
            card_seg(state, *source_id),
            text(" replacement applied: "),
            text(event_type),
        ],

        GameEvent::CrimeCommitted { player_id } => {
            vec![player_seg(state, *player_id), text(" commits a crime")]
        }

        GameEvent::PlayerLost { player_id } => {
            vec![player_seg(state, *player_id), text(" loses the game")]
        }

        GameEvent::PlayerEliminated { player_id } => {
            vec![player_seg(state, *player_id), text(" is eliminated")]
        }

        GameEvent::MulliganStarted => vec![text("Mulligan phase begins")],

        GameEvent::GameOver { winner } => match winner {
            Some(pid) => vec![
                text("Game over — "),
                player_seg(state, *pid),
                text(" wins!"),
            ],
            None => vec![text("Game over — Draw")],
        },

        GameEvent::MonarchChanged { player_id } => {
            vec![player_seg(state, *player_id), text(" becomes the monarch")]
        }

        GameEvent::CityBlessingGained { player_id } => {
            vec![
                player_seg(state, *player_id),
                text(" gets the city's blessing"),
            ]
        }

        GameEvent::DieRolled {
            player_id,
            sides,
            result,
        } => vec![
            player_seg(state, *player_id),
            text(" rolls a d"),
            num(*sides as i32),
            text(": "),
            num(*result as i32),
        ],

        GameEvent::CoinFlipped { player_id, won } => vec![
            player_seg(state, *player_id),
            text(" flips a coin: "),
            text(if *won { "wins" } else { "loses" }),
        ],

        GameEvent::RingTemptsYou { player_id } => {
            vec![text("The Ring tempts "), player_seg(state, *player_id)]
        }

        GameEvent::CreatureExploited {
            exploiter,
            sacrificed,
        } => vec![
            card_seg(state, *exploiter),
            text(" exploits "),
            card_seg(state, *sacrificed),
        ],

        GameEvent::Firebend {
            source_id,
            controller,
        } => vec![
            card_seg(state, *source_id),
            text(" firebends ("),
            player_seg(state, *controller),
            text(")"),
        ],

        GameEvent::Airbend {
            source_id,
            controller,
        } => vec![
            card_seg(state, *source_id),
            text(" airbends ("),
            player_seg(state, *controller),
            text(")"),
        ],

        GameEvent::Earthbend {
            source_id,
            controller,
        } => vec![
            card_seg(state, *source_id),
            text(" earthbends ("),
            player_seg(state, *controller),
            text(")"),
        ],

        GameEvent::Waterbend {
            source_id,
            controller,
        } => vec![
            card_seg(state, *source_id),
            text(" waterbends ("),
            player_seg(state, *controller),
            text(")"),
        ],

        GameEvent::CompanionRevealed {
            player, card_name, ..
        } => vec![
            player_seg(state, *player),
            text(" reveals "),
            text(card_name),
            text(" as their companion"),
        ],

        GameEvent::CompanionMovedToHand {
            player, card_name, ..
        } => vec![
            player_seg(state, *player),
            text(" puts their companion "),
            text(card_name),
            text(" into their hand"),
        ],

        GameEvent::EnergyChanged { player, delta } => {
            if *delta > 0 {
                vec![
                    player_seg(state, *player),
                    text(" gets "),
                    num(*delta),
                    text(" energy"),
                ]
            } else {
                vec![
                    player_seg(state, *player),
                    text(" pays "),
                    num(-*delta),
                    text(" energy"),
                ]
            }
        }

        GameEvent::PlayerCounterChanged {
            player,
            counter_kind,
            delta,
        } => vec![
            player_seg(state, *player),
            text(&format!(
                " gets {} {} counter{}",
                delta,
                counter_kind,
                if *delta != 1 { "s" } else { "" }
            )),
        ],

        GameEvent::ManaExpended {
            player_id,
            new_cumulative,
            ..
        } => vec![
            player_seg(state, *player_id),
            text(&format!(" expended (cumulative {})", new_cumulative)),
        ],

        GameEvent::PowerToughnessChanged {
            object_id,
            power,
            toughness,
            power_delta,
            toughness_delta,
        } => vec![
            card_seg(state, *object_id),
            text(&format!(
                " is now {}/{} ({:+}/{:+})",
                power, toughness, power_delta, toughness_delta
            )),
        ],

        GameEvent::VehicleCrewed {
            vehicle_id,
            creatures,
        } => {
            let mut segs = vec![card_seg(state, *vehicle_id), text(" crewed by ")];
            for (i, cid) in creatures.iter().enumerate() {
                if i > 0 {
                    segs.push(text(", "));
                }
                segs.push(card_seg(state, *cid));
            }
            segs
        }
        GameEvent::Stationed {
            spacecraft_id,
            creature_id,
            counters_added,
        } => vec![
            card_seg(state, *spacecraft_id),
            text(" stationed by "),
            card_seg(state, *creature_id),
            text(" (+"),
            num(*counters_added as i32),
            text(" charge)"),
        ],
        GameEvent::Saddled {
            mount_id,
            creatures,
        } => {
            let mut segs = vec![card_seg(state, *mount_id), text(" saddled by ")];
            for (i, cid) in creatures.iter().enumerate() {
                if i > 0 {
                    segs.push(text(", "));
                }
                segs.push(card_seg(state, *cid));
            }
            segs
        }
        GameEvent::RoomEntered { .. } => vec![text("Room entered")],
        GameEvent::DungeonCompleted { .. } => vec![text("Dungeon completed")],
        GameEvent::InitiativeTaken { .. } => vec![text("Initiative taken")],
        GameEvent::Clash { .. } => vec![text("Clash")],
        GameEvent::VoteCast { voter, choice, .. } => {
            vec![player_seg(state, *voter), text(" voted "), text(choice)]
        }
        GameEvent::VoteResolved { tallies, .. } => {
            let mut segs = vec![text("Vote resolved: ")];
            for (i, (label, count)) in tallies.iter().enumerate() {
                if i > 0 {
                    segs.push(text(", "));
                }
                segs.push(text(label));
                segs.push(text(": "));
                segs.push(text(&count.to_string()));
            }
            segs
        }
        GameEvent::XValueChosen { value, .. } => {
            vec![text("Chose X = "), text(&value.to_string())]
        }
        GameEvent::CombatTaxPaid {
            player,
            total_mana_value,
        } => vec![
            player_seg(state, *player),
            text(" paid combat tax ("),
            num(*total_mana_value as i32),
            text(" mana)"),
        ],
        GameEvent::CombatTaxDeclined { player, dropped } => vec![
            player_seg(state, *player),
            text(" declined combat tax ("),
            num(dropped.len() as i32),
            text(" creature(s) dropped)"),
        ],
        GameEvent::CascadeMissed {
            controller,
            exiled_count,
            ..
        } => vec![
            player_seg(state, *controller),
            text(" cascaded but found no eligible card ("),
            num(*exiled_count as i32),
            text(" cards exiled)"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;

    #[test]
    fn spell_cast_resolves_card_name() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            crate::types::zones::Zone::Stack,
        );
        let event = GameEvent::SpellCast {
            card_id: CardId(1),
            controller: PlayerId(0),
            object_id: id,
        };
        let entries = resolve_log_entries(&[event], &state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, LogCategory::Stack);
        // Verify card name is resolved
        let has_card_name = entries[0]
            .segments
            .iter()
            .any(|s| matches!(s, LogSegment::CardName { name, .. } if name == "Lightning Bolt"));
        assert!(
            has_card_name,
            "Expected CardName segment with 'Lightning Bolt'"
        );
    }

    #[test]
    fn damage_dealt_non_combat_is_life_category() {
        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        assert_eq!(categorize(&event), LogCategory::Life);
    }

    #[test]
    fn damage_dealt_combat_is_combat_category() {
        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        assert_eq!(categorize(&event), LogCategory::Combat);
    }

    #[test]
    fn player_name_defaults_to_player_n() {
        let state = GameState::new_two_player(42);
        let name = resolve_player_name(&state, PlayerId(0));
        assert_eq!(name, "Player 1");
    }

    #[test]
    fn player_name_uses_log_player_names() {
        let mut state = GameState::new_two_player(42);
        state.log_player_names = vec!["Alice".to_string(), "Bob".to_string()];
        assert_eq!(resolve_player_name(&state, PlayerId(0)), "Alice");
        assert_eq!(resolve_player_name(&state, PlayerId(1)), "Bob");
    }

    #[test]
    fn unknown_object_falls_back_gracefully() {
        let state = GameState::new_two_player(42);
        let name = resolve_object_name(&state, ObjectId(999));
        assert_eq!(name, "(unknown #999)");
    }

    #[test]
    fn lki_name_fallback_works() {
        let mut state = GameState::new_two_player(42);
        state.lki_cache.insert(
            ObjectId(42),
            crate::types::game_state::LKISnapshot {
                name: "Grizzly Bears".to_string(),
                power: Some(2),
                toughness: Some(2),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                counters: HashMap::new(),
            },
        );
        assert_eq!(resolve_object_name(&state, ObjectId(42)), "Grizzly Bears");
    }

    #[test]
    fn life_gained_segments() {
        let state = GameState::new_two_player(42);
        let segs = format_segments(
            &GameEvent::LifeChanged {
                player_id: PlayerId(0),
                amount: 3,
            },
            &state,
        );
        assert!(segs
            .iter()
            .any(|s| matches!(s, LogSegment::Text(t) if t == " gains ")));
    }

    #[test]
    fn life_lost_segments() {
        let state = GameState::new_two_player(42);
        let segs = format_segments(
            &GameEvent::LifeChanged {
                player_id: PlayerId(0),
                amount: -3,
            },
            &state,
        );
        assert!(segs
            .iter()
            .any(|s| matches!(s, LogSegment::Text(t) if t == " loses ")));
        assert!(segs.iter().any(|s| matches!(s, LogSegment::Number(3))));
    }

    #[test]
    fn all_event_variants_produce_segments() {
        // Ensure no event variant panics during formatting
        let state = GameState::new_two_player(42);
        let events = vec![
            GameEvent::GameStarted,
            GameEvent::TurnStarted {
                player_id: PlayerId(0),
                turn_number: 1,
            },
            GameEvent::PhaseChanged {
                phase: crate::types::phase::Phase::Untap,
            },
            GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            },
            GameEvent::MulliganStarted,
            GameEvent::GameOver {
                winner: Some(PlayerId(0)),
            },
            GameEvent::GameOver { winner: None },
            GameEvent::PlayerLost {
                player_id: PlayerId(0),
            },
            GameEvent::PlayerEliminated {
                player_id: PlayerId(0),
            },
            GameEvent::MonarchChanged {
                player_id: PlayerId(0),
            },
            GameEvent::DieRolled {
                player_id: PlayerId(0),
                sides: 20,
                result: 17,
            },
            GameEvent::CoinFlipped {
                player_id: PlayerId(0),
                won: true,
            },
            GameEvent::RingTemptsYou {
                player_id: PlayerId(0),
            },
            GameEvent::CrimeCommitted {
                player_id: PlayerId(0),
            },
            GameEvent::DayNightChanged {
                new_state: "Day".to_string(),
            },
            GameEvent::TokenCreated {
                object_id: ObjectId(1),
                name: "Zombie".to_string(),
            },
            GameEvent::PowerToughnessChanged {
                object_id: ObjectId(1),
                power: 4,
                toughness: 5,
                power_delta: 2,
                toughness_delta: 2,
            },
        ];
        let entries = resolve_log_entries(&events, &state);
        assert_eq!(entries.len(), events.len());
        for entry in &entries {
            assert!(
                !entry.segments.is_empty(),
                "Every event should produce at least one segment"
            );
        }
    }

    #[test]
    fn roundtrip_serialization() {
        let entry = GameLogEntry {
            seq: 0,
            turn: 1,
            phase: crate::types::phase::Phase::PreCombatMain,
            category: LogCategory::Stack,
            segments: vec![
                LogSegment::Text("casts ".to_string()),
                LogSegment::CardName {
                    name: "Bolt".to_string(),
                    object_id: ObjectId(5),
                },
            ],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: GameLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }
}

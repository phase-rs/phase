use std::collections::{HashMap, HashSet};

use crate::game::combat::AttackTarget;
use crate::game::planechase::PlanarDieFace;
use crate::types::ability::{AbilityTag, TargetRef};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, ZoneChangeRecord};
use crate::types::identifiers::ObjectId;
use crate::types::log::{
    GameLogEntry, LogBoundary, LogCategory, LogImportance, LogPresentation, LogSegment, LogTone,
    LogVisibility,
};
use crate::types::mana::{ManaColor, ManaType};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{ResolvedRulesCommand, RulesExecutionNodeRef};
use crate::types::stickers::StickerKind;
use crate::types::zones::Zone;

/// Resolve a batch of events into structured log entries.
/// Events that could leak hidden information are tagged for an explicit diagnostic opt-in.
pub fn resolve_log_entries(
    events: &[GameEvent],
    before: &GameState,
    after: &GameState,
) -> Vec<GameLogEntry> {
    let has_game_start = events
        .iter()
        .any(|event| matches!(event, GameEvent::GameStarted));
    let mut cursor = if has_game_start {
        LogCursor::pregame()
    } else {
        LogCursor {
            turn: before.turn_number,
            phase: before.phase,
        }
    };

    let batch = BatchIndex::new(events, after);
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            cursor.apply(event);
            (!should_exclude_event(event)
                && !is_redundant_log_event(events, index)
                && !is_player_leave_move(events, index, &batch)
                && !is_concealed_move(events, index, &batch, after))
            .then(|| {
                let mut segments = format_segments(event, after);
                name_at_event_time(&mut segments, &batch, index + 1);
                (!segments.is_empty()).then(|| GameLogEntry {
                    seq: 0, // Assigned by frontend
                    turn: cursor.turn,
                    phase: cursor.phase,
                    category: categorize(event),
                    segments,
                    presentation: presentation(event),
                })
            })?
        })
        .collect()
}

/// Prefer source-aware damage rows over derivative life-loss rows. Toxic's
/// poison-counter event and its replacement-pipeline bookkeeping may sit between
/// damage's life-loss consequence and its source-aware event; no effect-resolution
/// boundary is skipped. `apply_damage_after_replacement` emits this exact sequence,
/// while separate chained instructions each emit `EffectResolved` before the next
/// instruction begins, so unrelated life loss is not hidden by later damage. A tagged
/// activation is narrated once, by its keyword event, which emitters push immediately after
/// the generic one.
fn is_redundant_log_event(events: &[GameEvent], index: usize) -> bool {
    match events.get(index) {
        Some(GameEvent::LifeChanged {
            player_id, amount, ..
        }) if *amount < 0 => {
            let mut next_index = index + 1;
            let mut poison_seen = false;
            loop {
                match events.get(next_index) {
                    Some(GameEvent::ReplacementApplied { .. }) => next_index += 1,
                    Some(GameEvent::PlayerCounterChanged {
                        player,
                        counter_kind: crate::types::player::PlayerCounterKind::Poison,
                        delta,
                    }) if !poison_seen && player == player_id && *delta > 0 => {
                        poison_seen = true;
                        next_index += 1;
                    }
                    _ => break,
                }
            }
            matches!(
                events.get(next_index),
                Some(GameEvent::DamageDealt {
                    target: TargetRef::Player(damaged_player),
                    amount: damage,
                    ..
                }) if damaged_player == player_id && *damage == amount.unsigned_abs()
            )
        }
        Some(GameEvent::CombatDamageDealtToPlayer {
            player_id,
            source_amounts,
            ..
        }) => {
            let group_start = events[..index]
                .iter()
                .rposition(|event| {
                    matches!(
                        event,
                        GameEvent::CombatDamageDealtToPlayer {
                            player_id: previous_player,
                            ..
                        } if previous_player == player_id
                    )
                })
                .map_or(0, |previous_summary| previous_summary + 1);
            let mut source_rows = events[group_start..index]
                .iter()
                .filter_map(|event| match event {
                    GameEvent::DamageDealt {
                        source_id,
                        target: TargetRef::Player(damaged_player),
                        amount,
                        is_combat: true,
                        ..
                    } if damaged_player == player_id => Some((*source_id, *amount)),
                    _ => None,
                })
                .collect::<Vec<_>>();

            !source_amounts.is_empty()
                && source_amounts.iter().all(|summary_row| {
                    let Some(matched) = source_rows
                        .iter()
                        .position(|source_row| source_row == summary_row)
                    else {
                        return false;
                    };
                    source_rows.remove(matched);
                    true
                })
        }
        Some(GameEvent::AbilityActivated {
            player_id,
            source_id,
            ..
        }) => matches!(
            events.get(index + 1),
            Some(GameEvent::KeywordAbilityActivated {
                player_id: keyword_player,
                source_id: keyword_source,
                ..
            }) if keyword_player == player_id && keyword_source == source_id
        ),
        _ => false,
    }
}

/// A batch `ZoneChanged` as (position, origin, record).
type BatchMove<'a> = (usize, Option<Zone>, &'a ZoneChangeRecord);

/// Batch look-ups gathered once, so no per-event check rescans the batch or the journal.
struct BatchIndex<'a> {
    /// Each object's moves, ascending by position.
    moves: HashMap<ObjectId, Vec<BatchMove<'a>>>,
    turn_starts: Vec<usize>,
    eliminations: Vec<(usize, PlayerId)>,
    /// The event carries no incarnation, so the turn zone-change index tells repeated
    /// identical moves apart.
    player_leave_moves: HashSet<(ObjectId, Zone, Zone, usize)>,
}

impl<'a> BatchIndex<'a> {
    fn new(events: &'a [GameEvent], state: &GameState) -> Self {
        let mut batch = Self {
            moves: HashMap::new(),
            turn_starts: Vec::new(),
            eliminations: Vec::new(),
            player_leave_moves: state
                .resolved_rules_journal
                .entries()
                .iter()
                .filter_map(|entry| match entry.command.as_ref() {
                    Some(ResolvedRulesCommand::ZoneChange(command))
                        if matches!(command.cause, RulesExecutionNodeRef::PlayerLeave(_)) =>
                    {
                        Some((
                            command.object.object_id,
                            command.from,
                            command.to,
                            command.turn_zone_change_index,
                        ))
                    }
                    _ => None,
                })
                .collect(),
        };
        for (position, event) in events.iter().enumerate() {
            match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from,
                    record,
                    ..
                } => batch.moves.entry(*object_id).or_default().push((
                    position,
                    *from,
                    record.as_ref(),
                )),
                GameEvent::TurnStarted { .. } => batch.turn_starts.push(position),
                GameEvent::PlayerEliminated { player_id } => {
                    batch.eliminations.push((position, *player_id))
                }
                _ => {}
            }
        }
        batch
    }

    fn next_move(&self, object_id: ObjectId, from_index: usize) -> Option<&BatchMove<'a>> {
        let moves = self.moves.get(&object_id)?;
        moves.get(moves.partition_point(|&(position, ..)| position < from_index))
    }
}

fn departed_face_down(record: &ZoneChangeRecord) -> bool {
    record
        .trigger_source_context()
        .is_some_and(|context| context.face_down)
}

/// CR 400.2 + CR 406.3 + CR 708.9: whether the card's face was public as it left `from`.
fn departed_face_up(from: Zone, record: &ZoneChangeRecord) -> bool {
    from.is_public()
        && (from == Zone::Battlefield
            || (from == Zone::Stack && record.to_zone != Zone::Battlefield)
            || !departed_face_down(record))
}

/// Face-down status in exile is applied after the move is recorded.
fn arrived_face_down(
    batch: &BatchIndex,
    object_id: ObjectId,
    index: usize,
    after: &GameState,
) -> bool {
    match batch.next_move(object_id, index + 1) {
        Some((_, _, record)) => departed_face_down(record),
        None => after
            .objects
            .get(&object_id)
            .is_some_and(|obj| obj.face_down),
    }
}

/// CR 400.2 + CR 406.3: a move is narrated only if the card was face up in a public zone on one
/// side of it.
fn is_concealed_move(
    events: &[GameEvent],
    index: usize,
    batch: &BatchIndex,
    after: &GameState,
) -> bool {
    let Some(GameEvent::ZoneChanged {
        object_id,
        from: Some(from),
        to,
        record,
    }) = events.get(index)
    else {
        return false;
    };
    !departed_face_up(*from, record)
        && !(to.is_public() && !arrived_face_down(batch, *object_id, index, after))
}

/// CR 800.4a: whether this hidden-origin move is the leaving-player sweep; the journal holding
/// that cause is cleared at each turn start, so a move that a later `TurnStarted` follows is
/// judged from the batch.
fn is_player_leave_move(events: &[GameEvent], index: usize, batch: &BatchIndex) -> bool {
    let Some(GameEvent::ZoneChanged {
        object_id,
        from: Some(from),
        to,
        record,
    }) = events.get(index)
    else {
        return false;
    };
    if from.is_public() {
        return false;
    }
    let next_turn_start = batch.turn_starts.get(
        batch
            .turn_starts
            .partition_point(|&position| position <= index),
    );
    match next_turn_start {
        // Also hides the owner's own face-up exile earlier in that turn segment, since the
        // journal that told them apart is gone.
        Some(&turn_start) => {
            *to == Zone::Exile
                && batch.eliminations.iter().any(|&(position, player_id)| {
                    player_id == record.owner && (index..turn_start).contains(&position)
                })
        }
        None => batch.player_leave_moves.contains(&(
            *object_id,
            *from,
            *to,
            record.turn_zone_change_index,
        )),
    }
}

/// CR 400.7: a card's name can change as it moves, so a card cited before a later move in the
/// batch takes that move's recorded name, if the move left a public zone face up (CR 400.2); a
/// card that left one face down had no name (CR 406.3a).
fn name_at_event_time(segments: &mut [LogSegment], batch: &BatchIndex, from_index: usize) {
    for segment in segments {
        let LogSegment::CardName { name, object_id } = segment else {
            continue;
        };
        if let Some(&(_, Some(from), record)) = batch.next_move(*object_id, from_index) {
            if departed_face_up(from, record) {
                record.name.clone_into(name);
            } else if from.is_public() {
                name.clear();
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LogCursor {
    turn: u32,
    phase: Phase,
}

impl LogCursor {
    fn pregame() -> Self {
        Self {
            turn: 0,
            phase: Phase::Untap,
        }
    }

    fn apply(&mut self, event: &GameEvent) {
        match event {
            GameEvent::GameStarted => *self = Self::pregame(),
            GameEvent::TurnStarted { turn_number, .. } => {
                self.turn = *turn_number;
                self.phase = Phase::Untap;
            }
            GameEvent::PhaseChanged { phase } => self.phase = *phase,
            _ => {}
        }
    }
}

fn presentation(event: &GameEvent) -> LogPresentation {
    LogPresentation {
        importance: importance(event),
        tone: tone(event),
        boundary: boundary(event),
        visibility: visibility(event),
    }
}

fn importance(event: &GameEvent) -> LogImportance {
    match event {
        GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => LogImportance::Diagnostic,
        GameEvent::GameStarted
        | GameEvent::GameOver { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::PlayerEliminated { .. }
        | GameEvent::TurnStarted { .. }
        | GameEvent::SpellCast { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::LandPlayed { .. }
        | GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::DamageDealt { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. }
        | GameEvent::LifeChanged { .. }
        | GameEvent::ManaBurn { .. }
        | GameEvent::CreatureDestroyed { .. }
        | GameEvent::PermanentSacrificed { .. }
        | GameEvent::TokenCreated { .. }
        | GameEvent::ObjectConjured { .. } => LogImportance::Essential,
        GameEvent::PhaseChanged { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::ControllerChanged { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::Flipped { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. }
        | GameEvent::Discarded { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::CardsRevealed { .. }
        | GameEvent::ChosenNumbersRevealed { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::SpeedChanged { .. }
        // CR 309.4c: Entering a room fires that room's ability. Most entries are
        // automatic (single-arrow rooms, and the topmost room on entering a
        // dungeon), so the timeline is the only place a player learns which room
        // they landed in and what it does.
        | GameEvent::RoomEntered { .. }
        | GameEvent::ArmyAmassed { .. } => LogImportance::Context,
        // A countered spell or prevented damage is the decisive outcome of an
        // otherwise-visible action, so Timeline must not make that outcome
        // disappear behind the Details view.
        GameEvent::DamagePrevented { .. } | GameEvent::SpellCountered { .. } => {
            LogImportance::Context
        }
        // The remaining variants are deliberately listed rather than covered by a
        // wildcard. Adding a GameEvent must require an explicit presentation policy.
        // CR 701.17a + CR 701.17c: never narrated, because the paired `ZoneChanged`
        // already names the milled card and this event exists for mill triggers.
        GameEvent::Milled { .. }
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::ExtraTurnCreated { .. }
        | GameEvent::PriorityPassed { .. }
        | GameEvent::Mutated { .. }
        | GameEvent::Augmented { .. }
        | GameEvent::XValueChosen { .. }
        | GameEvent::ZoneChanged { .. }
        | GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaAbilityProduced { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaRecolored { .. }
        | GameEvent::PermanentTapped { .. }
        | GameEvent::CreatureExerted { .. }
        | GameEvent::CreatureEnlisted { .. }
        | GameEvent::Foretold { .. }
        | GameEvent::BecameForetold { .. }
        | GameEvent::MulliganStarted
        | GameEvent::CardsDrawn { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::BecomesPlotted { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::StackResolved { .. }
        // CR 714.2: bookkeeping the engine publishes so meta-triggers can
        // observe a chapter ability finishing; the chapter's own effects carry
        // the player-visible signal.
        | GameEvent::SagaChapterAbilityResolved { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::ResolutionHalted { .. }
        | GameEvent::ObjectIntensified { .. }
        | GameEvent::Evolved { .. }
        | GameEvent::Unattached { .. }
        | GameEvent::ContinuousEffectEnded { .. }
        | GameEvent::AttackerBecameBlockedByEffect { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
        | GameEvent::CombatTaxPaid { .. }
        | GameEvent::CombatTaxDeclined { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::EffectResolved { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::CascadeMissed { .. }
        | GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::EnduringStoryGained { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::StartingPlayerContest { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. }
        | GameEvent::SchemeSetInMotion { .. }
        | GameEvent::SchemeAbandoned { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::AttractionOpened { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::StickerPlaced { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::AttractionVisited { .. }
        | GameEvent::ContraptionCranked { .. }
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
        | GameEvent::Specialized { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. } => LogImportance::Detail,
    }
}

fn tone(event: &GameEvent) -> LogTone {
    match event {
        GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => LogTone::Diagnostic,
        GameEvent::LifeChanged { amount, .. } if *amount > 0 => LogTone::Positive,
        GameEvent::TokenCreated { .. }
        | GameEvent::ObjectConjured { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::EnduringStoryGained { .. }
        | GameEvent::MonarchChanged { .. }
        | GameEvent::InitiativeTaken { .. } => LogTone::Positive,
        GameEvent::DamageDealt { .. }
        | GameEvent::DamagePrevented { .. }
        | GameEvent::CreatureDestroyed { .. }
        | GameEvent::PermanentSacrificed { .. }
        | GameEvent::SpellCountered { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::PlayerEliminated { .. } => LogTone::Negative,
        GameEvent::LifeChanged { amount, .. } if *amount < 0 => LogTone::Negative,
        // Mana burn only ever costs life.
        GameEvent::ManaBurn { .. } => LogTone::Negative,
        GameEvent::SpellCast { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::AttackerBecameBlockedByEffect { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
        | GameEvent::CombatTaxPaid { .. }
        | GameEvent::CombatTaxDeclined { .. }
        | GameEvent::CreatureExerted { .. }
        | GameEvent::CreatureEnlisted { .. }
        | GameEvent::SpeedChanged { .. }
        | GameEvent::ArmyAmassed { .. }
        | GameEvent::DieRolled { .. }
        | GameEvent::CoinFlipped { .. }
        | GameEvent::RingTemptsYou { .. }
        | GameEvent::Firebend { .. }
        | GameEvent::Airbend { .. }
        | GameEvent::Earthbend { .. }
        | GameEvent::Waterbend { .. }
        | GameEvent::Clash { .. }
        | GameEvent::VoteCast { .. }
        | GameEvent::VoteResolved { .. } => LogTone::Informational,
        // CR 701.17a + CR 701.17c: never narrated, because the paired `ZoneChanged`
        // already names the milled card and this event exists for mill triggers.
        GameEvent::Milled { .. }
        | GameEvent::LifeChanged { .. }
        | GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::TurnStarted { .. }
        | GameEvent::ExtraTurnCreated { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PriorityPassed { .. }
        | GameEvent::Mutated { .. }
        | GameEvent::Augmented { .. }
        | GameEvent::XValueChosen { .. }
        | GameEvent::ZoneChanged { .. }
        | GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaAbilityProduced { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaRecolored { .. }
        | GameEvent::PermanentTapped { .. }
        | GameEvent::Foretold { .. }
        | GameEvent::BecameForetold { .. }
        | GameEvent::MulliganStarted
        | GameEvent::CardsDrawn { .. }
        | GameEvent::CardDrawn { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::BecomesPlotted { .. }
        | GameEvent::LandPlayed { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::StackResolved { .. }
        // CR 714.2: neither good nor bad news on its own — the drain or token
        // the observing trigger produces is what carries tone.
        | GameEvent::SagaChapterAbilityResolved { .. }
        | GameEvent::Discarded { .. }
        | GameEvent::Cycled { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::GameOver { .. }
        | GameEvent::ResolutionHalted { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::ObjectIntensified { .. }
        | GameEvent::Evolved { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::ControllerChanged { .. }
        | GameEvent::EffectResolved { .. }
        | GameEvent::Unattached { .. }
        | GameEvent::ContinuousEffectEnded { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::Transformed { .. }
        | GameEvent::Flipped { .. }
        | GameEvent::Specialized { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. }
        | GameEvent::CardsRevealed { .. }
        | GameEvent::ChosenNumbersRevealed { .. }
        | GameEvent::CombatDamageDealtToPlayer { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::CascadeMissed { .. }
        | GameEvent::StartingPlayerContest { .. }
        | GameEvent::RoomEntered { .. }
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. }
        | GameEvent::SchemeSetInMotion { .. }
        | GameEvent::SchemeAbandoned { .. }
        | GameEvent::AttractionOpened { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::StickerPlaced { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::AttractionVisited { .. }
        | GameEvent::ContraptionCranked { .. }
        | GameEvent::CompanionRevealed { .. }
        | GameEvent::CompanionMovedToHand { .. }
        | GameEvent::EnergyChanged { .. }
        | GameEvent::PlayerCounterChanged { .. }
        | GameEvent::ManaExpended { .. }
        | GameEvent::PlayerPerformedAction { .. } => LogTone::Neutral,
    }
}

fn boundary(event: &GameEvent) -> LogBoundary {
    match event {
        GameEvent::TurnStarted { .. } => LogBoundary::Turn,
        GameEvent::PhaseChanged { .. } => LogBoundary::Phase,
        _ => LogBoundary::None,
    }
}

fn visibility(event: &GameEvent) -> LogVisibility {
    match event {
        // Draws are intentionally retained for diagnostics, but normal logs
        // must not disclose an opponent or AI's private card flow.
        GameEvent::CardDrawn { .. } | GameEvent::CardsDrawn { .. } => {
            LogVisibility::HiddenInformation
        }
        _ => LogVisibility::Public,
    }
}

/// Returns true for events that should be excluded from log output.
/// Covers hidden-information leaks and low-signal stack bookkeeping.
fn should_exclude_event(event: &GameEvent) -> bool {
    match event {
        GameEvent::HiddenSearchViewed { .. } => true,
        // CR 701.17a + CR 701.17c: the paired `ZoneChanged` already names the milled
        // card; this event exists for mill triggers, so narrating it would duplicate
        // that line.
        GameEvent::Milled { .. } => true,
        GameEvent::ZoneChanged {
            from: Some(from),
            to,
            ..
        } => from == to,
        // PlayerPerformedAction { Draw } is an internal ledger signal consumed by
        // "for each player who drew a card this way" counting and
        // the player-action trigger index), not a user-facing event. Unlike
        // CardDrawn, which remains available as a HiddenInformation diagnostic,
        // excluding it keeps the visible log from narrating internal ledger events.
        GameEvent::PlayerPerformedAction {
            action: crate::types::events::PlayerActionKind::Draw,
            ..
        } => true,
        // StackPushed/StackResolved are low-signal bookkeeping —
        // the meaningful info is in SpellCast/AbilityActivated and EffectResolved
        GameEvent::StackPushed { .. } | GameEvent::StackResolved { .. } => true,
        // ReplacementApplied is engine bookkeeping. The resulting life,
        // counter, zone, or damage event carries the player-facing outcome.
        GameEvent::ReplacementApplied { .. } => true,
        // CR 714.2: the chapter-resolution notification exists so meta-triggers
        // can observe it; the player already saw the chapter ability itself
        // resolve. Same low-signal bookkeeping class as StackResolved.
        GameEvent::SagaChapterAbilityResolved { .. } => true,
        // CR 500.7: queue insertion is low-signal bookkeeping. The resolving
        // instruction and eventual `TurnStarted` event carry the narrative.
        GameEvent::ExtraTurnCreated { .. } => true,
        // `handle_empty_attackers` emits this bookkeeping event so the combat
        // pipeline can advance uniformly, but no creature attacked. It must
        // not be narrated as an attack against the default defender.
        GameEvent::AttackersDeclared { attacker_ids, .. } if attacker_ids.is_empty() => true,
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

fn attack_target_seg(state: &GameState, target: AttackTarget) -> LogSegment {
    match target {
        AttackTarget::Player(player_id) => player_seg(state, player_id),
        AttackTarget::Planeswalker(object_id) | AttackTarget::Battle(object_id) => {
            card_seg(state, object_id)
        }
    }
}

fn text(s: &str) -> LogSegment {
    LogSegment::Text(s.to_string())
}

fn num(n: i32) -> LogSegment {
    LogSegment::Number(n)
}

fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Untap => "Untap step",
        Phase::Upkeep => "Upkeep",
        Phase::Draw => "Draw step",
        Phase::PreCombatMain => "First main phase",
        Phase::BeginCombat => "Beginning of combat",
        Phase::DeclareAttackers => "Declare attackers",
        Phase::DeclareBlockers => "Declare blockers",
        Phase::CombatDamage => "Combat damage",
        Phase::EndCombat => "End of combat",
        Phase::PostCombatMain => "Second main phase",
        Phase::End => "End step",
        Phase::Cleanup => "Cleanup step",
    }
}

fn player_action_label(action: PlayerActionKind) -> &'static str {
    match action {
        PlayerActionKind::AcceptedOptionalEffect => "accepts an optional effect",
        PlayerActionKind::SearchedLibrary => "searches their library",
        PlayerActionKind::Scry => "scries",
        PlayerActionKind::Surveil => "surveils",
        PlayerActionKind::CollectEvidence => "collects evidence",
        PlayerActionKind::ShuffledLibrary => "shuffles their library",
        PlayerActionKind::Proliferate => "proliferates",
        PlayerActionKind::Investigate => "investigates",
        PlayerActionKind::Forage => "forages",
        PlayerActionKind::Draw => "draws",
    }
}

fn mana_type_symbol(mana_type: ManaType) -> &'static str {
    match mana_type {
        ManaType::White => "{W}",
        ManaType::Blue => "{U}",
        ManaType::Black => "{B}",
        ManaType::Red => "{R}",
        ManaType::Green => "{G}",
        ManaType::Colorless => "{C}",
    }
}

fn mana_color_name(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "white",
        ManaColor::Blue => "blue",
        ManaColor::Black => "black",
        ManaColor::Red => "red",
        ManaColor::Green => "green",
    }
}

fn planar_die_face_label(face: PlanarDieFace) -> &'static str {
    match face {
        PlanarDieFace::Planeswalk => "planeswalk",
        PlanarDieFace::Chaos => "chaos",
        PlanarDieFace::Blank => "blank",
    }
}

fn sticker_kind_label(kind: StickerKind) -> &'static str {
    match kind {
        StickerKind::Name => "name",
        StickerKind::Ability => "ability",
        StickerKind::PowerToughness => "power/toughness",
        StickerKind::Art => "art",
    }
}

/// Exhaustive categorization of game events.
fn categorize(event: &GameEvent) -> LogCategory {
    match event {
        // CR 701.17a + CR 701.17c: never narrated, because the paired `ZoneChanged`
        // already names the milled card and this event exists for mill triggers.
        GameEvent::Milled { .. }
        | GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::GameOver { .. }
        // CR 732.2: a halted runaway resolution is game-flow control, grouped
        // with GameOver under `Game` rather than object-state `State`.
        | GameEvent::ResolutionHalted { .. }
        | GameEvent::PlayerLost { .. }
        | GameEvent::PlayerEliminated { .. }
        // CR 103.1: grouped with the setup event MulliganStarted under `Game`
        // (not `Special` like in-game DieRolled) — it is game setup, not a
        // CR 706 die-roll log entry.
        | GameEvent::StartingPlayerContest { .. }
        | GameEvent::MulliganStarted => LogCategory::Game,

        GameEvent::TurnStarted { .. }
        | GameEvent::ExtraTurnCreated { .. }
        | GameEvent::PhaseChanged { .. }
        | GameEvent::PriorityPassed { .. } => LogCategory::Turn,

        GameEvent::SpellCast { .. }
        | GameEvent::SpellCopied { .. }
        | GameEvent::AbilityActivated { .. }
        | GameEvent::NinjutsuActivated { .. }
        | GameEvent::KeywordAbilityActivated { .. }
        | GameEvent::StackPushed { .. }
        | GameEvent::StackResolved { .. }
        // CR 714.2: a chapter ability finishing resolution is a stack event.
        | GameEvent::SagaChapterAbilityResolved { .. }
        | GameEvent::SpellCountered { .. } => LogCategory::Stack,

        GameEvent::AttackersDeclared { .. }
        | GameEvent::BlockersDeclared { .. }
        | GameEvent::AttackerBecameBlockedByEffect { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
        | GameEvent::CreatureExerted { .. }
        | GameEvent::CreatureEnlisted { .. }
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
        | GameEvent::CardsRevealed { .. }
        | GameEvent::ChosenNumbersRevealed { .. }
        | GameEvent::Foretold { .. }
        | GameEvent::BecameForetold { .. } => LogCategory::Zone,

        GameEvent::LifeChanged { .. } => LogCategory::Life,

        GameEvent::ManaAdded { .. }
        | GameEvent::TappedForMana { .. }
        | GameEvent::ManaAbilityProduced { .. }
        | GameEvent::ManaPoolEmptied { .. }
        | GameEvent::ManaRecolored { .. }
        // The mana-side explanation; the LifeChanged it causes is categorized Life.
        | GameEvent::ManaBurn { .. } => LogCategory::Mana,

        GameEvent::PermanentTapped { .. }
        | GameEvent::PermanentUntapped { .. }
        | GameEvent::PermanentPhasedOut { .. }
        | GameEvent::PermanentPhasedIn { .. }
        | GameEvent::PlayerPhasedOut { .. }
        | GameEvent::PlayerPhasedIn { .. }
        | GameEvent::DamageCleared { .. }
        | GameEvent::CounterAdded { .. }
        | GameEvent::ObjectIntensified { .. }
        | GameEvent::Evolved { .. }
        | GameEvent::CounterRemoved { .. }
        | GameEvent::ControllerChanged { .. }
        | GameEvent::Transformed { .. }
        // CR 710.4: flipping is an object-status change, grouped with transform
        // and face up/down.
        | GameEvent::Flipped { .. }
        | GameEvent::TurnedFaceUp { .. }
        | GameEvent::TurnedFaceDown { .. }
        | GameEvent::Regenerated { .. }
        | GameEvent::CreatureSuspected { .. }
        | GameEvent::CreatureNoLongerSuspected { .. }
        | GameEvent::Detained { .. }
        | GameEvent::BecamePrepared { .. }
        | GameEvent::BecameUnprepared { .. }
        | GameEvent::CaseSolved { .. }
        | GameEvent::ClassLevelGained { .. }
        | GameEvent::DayNightChanged { .. }
        | GameEvent::PowerToughnessChanged { .. }
        | GameEvent::VehicleCrewed { .. }
        | GameEvent::Stationed { .. }
        | GameEvent::Saddled { .. }
        // CR 702.140c + CR 730.2: a mutating creature spell merged with a permanent.
        | GameEvent::Mutated { .. }
        // Unstable Host/Augment: a card with augment combined with a Host creature.
        | GameEvent::Augmented { .. }
        | GameEvent::BecomesPlotted { .. } => LogCategory::State,

        GameEvent::SpeedChanged { .. } | GameEvent::ArmyAmassed { .. } => LogCategory::Special,

        GameEvent::TokenCreated { .. } | GameEvent::ObjectConjured { .. } => LogCategory::Token,

        GameEvent::EffectResolved { .. }
        | GameEvent::Unattached { .. }
        // CR 116.2c: a special action that ends a continuous effect is an
        // effect-level state change, grouped with the other effect events.
        | GameEvent::ContinuousEffectEnded { .. }
        | GameEvent::BecomesTarget { .. }
        | GameEvent::ReplacementApplied { .. }
        | GameEvent::CrimeCommitted { .. }
        | GameEvent::CascadeMissed { .. } => LogCategory::Trigger,

        GameEvent::CreatureDestroyed { .. } | GameEvent::PermanentSacrificed { .. } => {
            LogCategory::Destroy
        }

        GameEvent::CardPredicateGuessMade { .. }
        | GameEvent::DebugActionUsed { .. }
        | GameEvent::DebugPermissionGranted { .. }
        | GameEvent::DebugPermissionRevoked { .. } => LogCategory::Debug,

        GameEvent::MonarchChanged { .. }
        | GameEvent::CityBlessingGained { .. }
        | GameEvent::EnduringStoryGained { .. }
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
        | GameEvent::RoomDoorUnlocked { .. }
        | GameEvent::DungeonCompleted { .. }
        | GameEvent::Planeswalked { .. }
        | GameEvent::ChaosEnsued { .. }
        | GameEvent::PlanarDieRolled { .. }
        | GameEvent::SchemeSetInMotion { .. }
        | GameEvent::SchemeAbandoned { .. }
        | GameEvent::InitiativeTaken { .. }
        | GameEvent::AttractionOpened { .. }
        | GameEvent::ContraptionAssembled { .. }
        | GameEvent::StickerPlaced { .. }
        | GameEvent::AttractionsRolledToVisit { .. }
        | GameEvent::AttractionVisited { .. }
        | GameEvent::ContraptionCranked { .. }
        | GameEvent::Specialized { .. }
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
        GameEvent::HiddenSearchViewed { .. } => vec![],
        GameEvent::ExtraTurnCreated { .. } => vec![],
        // CR 701.17a + CR 701.17c: never narrated, because the paired `ZoneChanged`
        // already names the milled card and this event exists for mill triggers.
        GameEvent::Milled { .. } => vec![],

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
            vec![text(phase_label(*phase))]
        }

        GameEvent::PriorityPassed { player_id } => {
            vec![player_seg(state, *player_id), text(" passes priority")]
        }

        GameEvent::PlayerPerformedAction {
            player_id,
            action: crate::types::events::PlayerActionKind::Scry,
            look_count: Some(look_count),
            scry_bottom_count: Some(scry_bottom_count),
            ..
        } => vec![
            player_seg(state, *player_id),
            text(" scries "),
            num(*look_count as i32),
            text(": "),
            num(look_count.saturating_sub(*scry_bottom_count) as i32),
            text(" on top and "),
            num(*scry_bottom_count as i32),
            text(" on bottom"),
        ],
        GameEvent::PlayerPerformedAction {
            player_id, action, ..
        } => vec![
            player_seg(state, *player_id),
            text(" "),
            text(player_action_label(*action)),
        ],
        GameEvent::CardPredicateGuessMade {
            player_id,
            source_id,
            choice,
        } => {
            let mut segments = vec![
                player_seg(state, *player_id),
                text(" guesses "),
                text(choice),
            ];
            if let Some(source_id) = source_id {
                segments.push(text(" for "));
                segments.push(card_seg(state, *source_id));
            }
            segments
        }

        GameEvent::SpellCast {
            controller,
            object_id,
            ..
        } => vec![
            player_seg(state, *controller),
            text(" casts "),
            card_seg(state, *object_id),
        ],

        GameEvent::SpellCopied {
            controller,
            object_id,
            ..
        } => vec![
            player_seg(state, *controller),
            text(" copies "),
            card_seg(state, *object_id),
        ],

        GameEvent::AbilityActivated {
            player_id,
            source_id,
            ..
        } => vec![
            player_seg(state, *player_id),
            text(" activates ability: "),
            card_seg(state, *source_id),
        ],

        GameEvent::NinjutsuActivated {
            player_id,
            source_id,
        } => vec![
            player_seg(state, *player_id),
            text(" activates ninjutsu: "),
            card_seg(state, *source_id),
        ],

        GameEvent::KeywordAbilityActivated {
            ability_tag,
            player_id,
            source_id,
            ..
        } => {
            let label = match ability_tag {
                AbilityTag::Boast => " activates boast: ",
                AbilityTag::Evolve => " activates evolve: ",
                AbilityTag::Exhaust => " activates exhaust: ",
                AbilityTag::Outlast => " activates outlast: ",
                // CR 702.29c: Cycling emits a dedicated `GameEvent::Cycled`, not a
                // `KeywordAbilityActivated` event, so this arm is unreachable.
                AbilityTag::Cycling => " activates cycling: ",
                // CR 702.165a: Backup is a triggered ability — it never emits a
                // `KeywordAbilityActivated` event, so this arm is unreachable.
                AbilityTag::Backup => " activates backup: ",
                // CR 602.5b: Power-up activation.
                AbilityTag::PowerUp => " activates power-up: ",
                // CR 702.6a: Equip activation.
                AbilityTag::Equip => " activates equip: ",
                AbilityTag::Augment => " activates augment: ",
            };
            vec![
                player_seg(state, *player_id),
                text(label),
                card_seg(state, *source_id),
            ]
        }

        GameEvent::BecomesPlotted {
            object_id,
            player_id,
        } => vec![
            card_seg(state, *object_id),
            text(" becomes plotted for "),
            player_seg(state, *player_id),
        ],

        GameEvent::CreatureExerted { object_id } => {
            vec![card_seg(state, *object_id), text(" is exerted")]
        }

        GameEvent::CreatureEnlisted {
            attacker, tapped, ..
        } => vec![
            card_seg(state, *attacker),
            text(" enlists "),
            card_seg(state, *tapped),
        ],

        GameEvent::ArmyAmassed { object_id, .. } => {
            vec![card_seg(state, *object_id), text(" is amassed")]
        }

        GameEvent::StackPushed { object_id } => {
            vec![card_seg(state, *object_id), text(" added to stack")]
        }

        GameEvent::StackResolved { object_id } => {
            vec![card_seg(state, *object_id), text(" resolves")]
        }

        // CR 714.2: filtered out by `is_low_signal` above — the chapter
        // ability's own resolution line already told the player what happened.
        GameEvent::SagaChapterAbilityResolved { .. } => vec![],

        GameEvent::SpellCountered {
            object_id,
            countered_by,
            ..
        } => vec![
            card_seg(state, *countered_by),
            text(" counters "),
            card_seg(state, *object_id),
        ],

        GameEvent::Unattached {
            attachment_id,
            old_target,
        } => {
            let mut segments = vec![
                card_seg(state, *attachment_id),
                text(" becomes unattached from "),
            ];
            match old_target {
                TargetRef::Object(object_id) => segments.push(card_seg(state, *object_id)),
                TargetRef::Player(player_id) => segments.push(player_seg(state, *player_id)),
            }
            segments
        }

        // CR 116.2c: a player-visible special action with no log line would be a
        // defect. The group key is engine bookkeeping and is deliberately not
        // rendered — the source permanent is the player-meaningful identity.
        GameEvent::ContinuousEffectEnded {
            group: _,
            source_id,
            player,
        } => vec![
            player_seg(state, *player),
            text(" pays to end "),
            card_seg(state, *source_id),
            text("'s effect"),
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
            ..
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
            ..
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

        // CR 101.4: one line for the whole simultaneous reveal — the numbers
        // become public together, so rendering them per-player would imply an
        // ordering the rules do not have.
        GameEvent::ChosenNumbersRevealed { numbers } => {
            let mut segments = vec![text("Chosen numbers revealed: ")];
            for (index, (player, value)) in numbers.iter().enumerate() {
                if index > 0 {
                    segments.push(text(", "));
                }
                segments.push(player_seg(state, *player));
                segments.push(text(" "));
                segments.push(num(crate::game::arithmetic::u32_to_i32_saturating(*value)));
            }
            segments
        }

        GameEvent::LifeChanged {
            player_id, amount, ..
        } => {
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

        // Names the rule, not just the loss: the `LifeChanged` event that
        // follows says a player lost life, and only this says why.
        GameEvent::ManaBurn { player_id, amount } => vec![
            player_seg(state, *player_id),
            text(" loses "),
            num(*amount as i32),
            text(" life to mana burn"),
        ],

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
            attacks,
            ..
        } => {
            // The legacy fallback keeps pre-`attacks` snapshots legible. New
            // declarations preserve each attacker's actual target, which may be
            // a different player, planeswalker, or battle.
            let attack_targets: Vec<_> = if attacks.is_empty() {
                attacker_ids
                    .iter()
                    .copied()
                    .map(|attacker| (attacker, AttackTarget::Player(*defending_player)))
                    .collect()
            } else {
                attacks.clone()
            };
            let mut groups: Vec<(AttackTarget, Vec<ObjectId>)> = Vec::new();
            for (attacker, target) in attack_targets {
                if let Some((_, attackers)) =
                    groups.iter_mut().find(|(existing, _)| *existing == target)
                {
                    attackers.push(attacker);
                } else {
                    groups.push((target, vec![attacker]));
                }
            }

            let mut segs = Vec::new();
            for (group_index, (target, attackers)) in groups.iter().enumerate() {
                if group_index > 0 {
                    segs.push(text("; "));
                }
                for (attacker_index, attacker) in attackers.iter().enumerate() {
                    if attacker_index > 0 {
                        segs.push(text(if attacker_index + 1 == attackers.len() {
                            " and "
                        } else {
                            ", "
                        }));
                    }
                    segs.push(card_seg(state, *attacker));
                }
                segs.push(text(if attackers.len() == 1 {
                    " attacks "
                } else {
                    " attack "
                }));
                segs.push(attack_target_seg(state, *target));
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

        // CR 509.1h: an effect made an attacker become blocked (no blockers).
        GameEvent::AttackerBecameBlockedByEffect { attacker } => {
            vec![card_seg(state, *attacker), text(" becomes blocked")]
        }

        // CR 509.3d: a disambiguated single blocker/attacker pair from a
        // per-blocker filtered blocks-or-becomes-blocked firing.
        GameEvent::AttackerBecameBlockedByFilteredBlocker { attacker, blocker } => {
            vec![
                card_seg(state, *blocker),
                text(" blocks "),
                card_seg(state, *attacker),
            ]
        }

        GameEvent::CombatDamageDealtToPlayer {
            player_id,
            source_amounts,
            total_damage,
        } => vec![
            player_seg(state, *player_id),
            text(" is dealt "),
            num(*total_damage as i32),
            text(" combat damage by "),
            num(source_amounts.len() as i32),
            text(if source_amounts.len() == 1 {
                " creature"
            } else {
                " creatures"
            }),
        ],

        GameEvent::ManaAdded {
            source_id,
            mana_type,
            ..
        } => vec![
            card_seg(state, *source_id),
            text(" adds "),
            LogSegment::Mana(mana_type_symbol(*mana_type).to_string()),
            text(" mana"),
        ],
        // CR 500.5 + CR 703.4q: A unit was emptied from a pool at step end.
        GameEvent::ManaPoolEmptied {
            player_id, color, ..
        } => vec![
            player_seg(state, *player_id),
            text(" loses "),
            LogSegment::Mana(mana_type_symbol(*color).to_string()),
            text(" mana"),
        ],
        // CR 614.1a + CR 703.4q: A Transform handler recolored a unit at step end.
        GameEvent::ManaRecolored {
            player_id,
            from,
            to,
        } => vec![
            player_seg(state, *player_id),
            text("'s "),
            LogSegment::Mana(mana_type_symbol(*from).to_string()),
            text(" mana becomes "),
            LogSegment::Mana(mana_type_symbol(*to).to_string()),
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
            // CR 122.1: the log line names the counters and recipient; the placing
            // player is implied by the entry's stack/ability context, consistent
            // with every other counter-placement log line (actor deliberately
            // not surfaced).
            ..
        } => vec![
            num(*count as i32),
            text(" "),
            LogSegment::Keyword(counter_type.display_phrase().into_owned()),
            text(if *count == 1 {
                " counter on "
            } else {
                " counters on "
            }),
            card_seg(state, *object_id),
        ],

        GameEvent::ObjectIntensified { object_id, amount } => vec![
            card_seg(state, *object_id),
            text(" intensified by "),
            num(*amount as i32),
        ],

        GameEvent::Evolved { object_id } => {
            vec![card_seg(state, *object_id), text(" evolved")]
        }

        GameEvent::CounterRemoved {
            object_id,
            counter_type,
            count,
        } => vec![
            num(*count as i32),
            text(" "),
            LogSegment::Keyword(counter_type.display_phrase().into_owned()),
            text(if *count == 1 {
                " counter removed from "
            } else {
                " counters removed from "
            }),
            card_seg(state, *object_id),
        ],

        GameEvent::Transformed { object_id } => {
            vec![card_seg(state, *object_id), text(" transforms")]
        }

        // CR 710.4: the log names the permanent by its (now alternative,
        // CR 710.1b) characteristics, which `card_seg` reads live.
        GameEvent::Flipped { object_id } => {
            vec![card_seg(state, *object_id), text(" flips")]
        }

        GameEvent::Specialized { object_id, color } => {
            vec![
                card_seg(state, *object_id),
                text(" specializes into "),
                text(mana_color_name(*color)),
            ]
        }

        // CR 702.140c + CR 730.2: a mutating creature spell merged with a permanent.
        GameEvent::Mutated {
            merged_id,
            merging_id,
            ..
        } => vec![
            card_seg(state, *merging_id),
            text(" mutates onto "),
            card_seg(state, *merged_id),
        ],

        GameEvent::Augmented {
            merged_id,
            augmenting_id,
            ..
        } => vec![
            card_seg(state, *augmenting_id),
            text(" augments "),
            card_seg(state, *merged_id),
        ],

        GameEvent::TurnedFaceUp { object_id } => {
            vec![card_seg(state, *object_id), text(" is turned face up")]
        }

        GameEvent::TurnedFaceDown { object_id } => {
            vec![card_seg(state, *object_id), text(" is turned face down")]
        }

        GameEvent::Regenerated { object_id } => {
            vec![card_seg(state, *object_id), text(" regenerates")]
        }

        GameEvent::CreatureSuspected { object_id } => {
            vec![card_seg(state, *object_id), text(" becomes suspected")]
        }

        GameEvent::CreatureNoLongerSuspected { object_id } => {
            vec![card_seg(state, *object_id), text(" is no longer suspected")]
        }

        GameEvent::Detained { object_id } => {
            vec![card_seg(state, *object_id), text(" is detained")]
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

        GameEvent::TokenCreated {
            object_id, name, ..
        } => vec![
            LogSegment::CardName {
                name: name.clone(),
                object_id: *object_id,
            },
            text(" token is created"),
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

        GameEvent::ControllerChanged {
            object_id,
            old_controller,
            new_controller,
        } => vec![
            card_seg(state, *object_id),
            text(" changed controller from "),
            player_seg(state, *old_controller),
            text(" to "),
            player_seg(state, *new_controller),
        ],

        GameEvent::EffectResolved { source_id, .. } => {
            vec![card_seg(state, *source_id), text("'s effect resolves")]
        }

        GameEvent::BecomesTarget {
            target, source_id, ..
        } => {
            let mut segments = Vec::new();
            match target {
                TargetRef::Object(object_id) => segments.push(card_seg(state, *object_id)),
                TargetRef::Player(player_id) => segments.push(player_seg(state, *player_id)),
            }
            segments.push(text(" is targeted by "));
            segments.push(card_seg(state, *source_id));
            segments
        }

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

        // CR 103.1: concise one-line summary of the starting-player roll-off;
        // round-by-round detail lives in the structured event for the UI.
        GameEvent::StartingPlayerContest { winner, .. } => vec![
            player_seg(state, *winner),
            text(" wins the roll to take the first turn"),
        ],

        GameEvent::GameOver { winner } => match winner {
            Some(pid) => vec![
                text("Game over — "),
                player_seg(state, *pid),
                text(" wins!"),
            ],
            None => vec![text("Game over — Draw")],
        },

        // CR 732.2: engine-authored game-flow message — raw text, not t()-wrapped
        // (the i18n boundary keeps engine/log pass-through strings raw).
        GameEvent::ResolutionHalted { .. } => {
            vec![text("Resolution halted — possible mandatory loop")]
        }

        GameEvent::MonarchChanged { player_id } => {
            vec![player_seg(state, *player_id), text(" becomes the monarch")]
        }

        GameEvent::CityBlessingGained { player_id } => {
            vec![
                player_seg(state, *player_id),
                text(" gets the city's blessing"),
            ]
        }

        GameEvent::EnduringStoryGained { player_id } => {
            vec![
                player_seg(state, *player_id),
                text(" gains an enduring story"),
            ]
        }

        GameEvent::DieRolled {
            player_id,
            sides,
            result,
        } => match result {
            // CR 706: a numeric die roll renders its face value.
            Some(r) => vec![
                player_seg(state, *player_id),
                text(" rolls a d"),
                num(*sides as i32),
                text(": "),
                num(*r as i32),
            ],
            // CR 901.9d / CR 706.7: the symbolic planar die has no numeric face.
            None => vec![player_seg(state, *player_id), text(" rolls the planar die")],
        },

        GameEvent::CoinFlipped { player_id, won } => vec![
            player_seg(state, *player_id),
            text(" flips a coin: "),
            text(if *won { "wins" } else { "loses" }),
        ],

        GameEvent::RingTemptsYou { player_id, .. } => {
            vec![text("The Ring tempts "), player_seg(state, *player_id)]
        }

        GameEvent::CreatureExploited {
            exploiter,
            sacrificed,
            ..
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
        } => {
            let count = delta.unsigned_abs();
            if *delta > 0 {
                vec![
                    player_seg(state, *player),
                    text(&format!(
                        " gets {} {} counter{}",
                        count,
                        counter_kind,
                        if count != 1 { "s" } else { "" }
                    )),
                ]
            } else {
                vec![
                    player_seg(state, *player),
                    text(&format!(
                        " loses {} {} counter{}",
                        count,
                        counter_kind,
                        if count != 1 { "s" } else { "" }
                    )),
                ]
            }
        }

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
        // CR 309.4b-c: Name the room entered and what its room ability does.
        // Most room entries are automatic (single-arrow rooms, and the topmost
        // room on entering a dungeon), so the log is the only place a player
        // sees them.
        GameEvent::RoomEntered {
            player_id,
            dungeon,
            room_index,
            room_name,
        } => {
            let mut segs = vec![
                player_seg(state, *player_id),
                text(" entered "),
                text(room_name),
                text(" ("),
                text(&dungeon.to_string()),
                text(")"),
            ];
            let effect = crate::game::dungeon::room_text(*dungeon, *room_index);
            if !effect.is_empty() {
                segs.push(text(": "));
                segs.push(text(effect));
            }
            segs
        }
        GameEvent::RoomDoorUnlocked { .. } => vec![text("Room door unlocked")],
        GameEvent::DungeonCompleted { .. } => vec![text("Dungeon completed")],
        GameEvent::Planeswalked { .. } => vec![text("Planeswalked")],
        GameEvent::ChaosEnsued { .. } => vec![text("Chaos ensues")],
        GameEvent::PlanarDieRolled { face, .. } => {
            vec![
                text("The planar die lands on "),
                text(planar_die_face_label(*face)),
            ]
        }
        GameEvent::SchemeSetInMotion { scheme_id, .. } => {
            vec![text("Set scheme in motion: "), card_seg(state, *scheme_id)]
        }
        GameEvent::SchemeAbandoned { scheme_id, .. } => {
            vec![text("Abandoned scheme: "), card_seg(state, *scheme_id)]
        }
        GameEvent::InitiativeTaken { .. } => vec![text("Initiative taken")],
        GameEvent::AttractionOpened { object_id, .. } => {
            vec![text("Opened Attraction "), card_seg(state, *object_id)]
        }
        GameEvent::ContraptionAssembled {
            object_id,
            sprocket,
            ..
        } => vec![
            text("Assembled Contraption "),
            card_seg(state, *object_id),
            text(" onto sprocket "),
            text(&sprocket.to_string()),
        ],
        GameEvent::StickerPlaced {
            object_id, kind, ..
        } => vec![
            text("Placed "),
            text(sticker_kind_label(*kind)),
            text(" sticker on "),
            card_seg(state, *object_id),
        ],
        GameEvent::AttractionsRolledToVisit { rolls, .. } => {
            // CR 701.52a: ONE turn-based action, so ONE log line — a count-
            // raising replacement (CR 706.6) that leaves several surviving dice
            // lists them together rather than reporting the action twice.
            vec![
                text("Rolled "),
                text(
                    &rolls
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                text(" to visit Attractions"),
            ]
        }
        GameEvent::AttractionVisited {
            attraction_id,
            roll,
            ..
        } => {
            vec![
                text("Visited Attraction "),
                card_seg(state, *attraction_id),
                text(" (rolled "),
                text(&roll.to_string()),
                text(")"),
            ]
        }
        GameEvent::ContraptionCranked {
            contraption_id,
            sprocket,
            ..
        } => vec![
            text("Cranked Contraption "),
            card_seg(state, *contraption_id),
            text(" on sprocket "),
            text(&sprocket.to_string()),
        ],
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
            text(if dropped.len() == 1 {
                " creature dropped)"
            } else {
                " creatures dropped)"
            }),
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

        GameEvent::DebugActionUsed {
            player_id,
            description,
        } => vec![
            player_seg(state, *player_id),
            text(" used debug: "),
            text(description),
        ],
        GameEvent::DebugPermissionGranted { host, player_id } => vec![
            player_seg(state, *host),
            text(" granted debug actions to "),
            player_seg(state, *player_id),
        ],
        GameEvent::DebugPermissionRevoked { host, player_id } => vec![
            player_seg(state, *host),
            text(" revoked debug actions from "),
            player_seg(state, *player_id),
        ],
        GameEvent::Foretold { player_id, .. } => {
            vec![player_seg(state, *player_id), text(" foretold a card")]
        }
        // CR 702.143d: an effect made an exiled card foretold (no foretelling
        // player — the card itself became foretold).
        GameEvent::BecameForetold { .. } => vec![text("An exiled card becomes foretold")],
        // CR 106.12a: `TappedForMana` is the per-resolution trigger event for
        // `TapsForMana` matchers. The per-unit `ManaAdded` events already
        // produce the user-facing "adds X mana" log lines, so this event is
        // internal plumbing and emits no segments of its own.
        GameEvent::TappedForMana { .. } | GameEvent::ManaAbilityProduced { .. } => vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::game::engine::{
        start_game, start_game_skip_mulligan, start_game_with_starting_player,
    };
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;

    /// CR 701.17a + CR 701.17c: the paired `ZoneChanged` names the milled card, so the
    /// trigger-facing mill event is dropped rather than narrated twice.
    #[test]
    fn milled_is_excluded_from_the_log() {
        let milled = GameEvent::Milled {
            player_id: PlayerId(0),
            object_id: ObjectId(7),
            to: crate::types::zones::Zone::Graveyard,
        };
        assert!(should_exclude_event(&milled));

        // Live control in the same invocation: a predicate stuck at `true`, or
        // one that never ran, cannot pass this leg.
        let cast = GameEvent::SpellCast {
            card_id: CardId(1),
            controller: PlayerId(0),
            object_id: ObjectId(7),
            cast_mana_value: None,
        };
        assert!(!should_exclude_event(&cast));
    }

    #[test]
    fn extra_turn_creation_is_excluded_but_turn_start_is_visible() {
        let state = GameState::new_two_player(42);
        let creation = GameEvent::ExtraTurnCreated {
            player_id: PlayerId(1),
            anchor: PlayerId(0),
        };
        let turn_started = GameEvent::TurnStarted {
            player_id: PlayerId(1),
            turn_number: 2,
        };

        assert_eq!(importance(&creation), LogImportance::Detail);
        assert_eq!(tone(&creation), LogTone::Neutral);
        assert_eq!(categorize(&creation), LogCategory::Turn);
        assert!(should_exclude_event(&creation));
        assert!(format_segments(&creation, &state).is_empty());
        assert!(resolve_log_entries(&[creation], &state, &state).is_empty());
        assert_eq!(
            resolve_log_entries(&[turn_started], &state, &state).len(),
            1
        );
    }

    #[test]
    fn empty_attack_declaration_is_excluded_from_the_log() {
        let no_attackers = GameEvent::AttackersDeclared {
            attacker_ids: vec![],
            defending_player: PlayerId(1),
            attacks: vec![],
            declaration_records: Vec::new(),
        };
        let attacker = GameEvent::AttackersDeclared {
            attacker_ids: vec![ObjectId(7)],
            defending_player: PlayerId(1),
            attacks: vec![],
            declaration_records: Vec::new(),
        };

        assert!(should_exclude_event(&no_attackers));
        assert!(!should_exclude_event(&attacker));
    }

    #[test]
    fn attack_log_uses_each_attackers_actual_target() {
        let mut state = GameState::new_two_player(42);
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Balduvian Bears".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let wolf = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Runeclaw Bear".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let gideon = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Gideon Jura".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let event = GameEvent::AttackersDeclared {
            attacker_ids: vec![bear, wolf],
            defending_player: PlayerId(1),
            attacks: vec![
                (bear, AttackTarget::Player(PlayerId(1))),
                (wolf, AttackTarget::Planeswalker(gideon)),
            ],
            declaration_records: Vec::new(),
        };

        assert_eq!(
            format_segments(&event, &state),
            vec![
                card_seg(&state, bear),
                text(" attacks "),
                player_seg(&state, PlayerId(1)),
                text("; "),
                card_seg(&state, wolf),
                text(" attacks "),
                card_seg(&state, gideon),
            ]
        );
    }

    #[test]
    fn countering_and_prevention_are_visible_in_timeline() {
        let countered = GameEvent::SpellCountered {
            object_id: ObjectId(7),
            countered_by: ObjectId(8),
            countered_by_controller: PlayerId(1),
        };
        let prevented = GameEvent::DamagePrevented {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
        };

        assert_eq!(importance(&countered), LogImportance::Context);
        assert_eq!(importance(&prevented), LogImportance::Context);
    }

    #[test]
    fn combat_damage_summary_keeps_the_actual_total() {
        let state = GameState::new_two_player(42);
        let event = GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(ObjectId(7), 3), (ObjectId(8), 4)],
            total_damage: 7,
        };

        assert_eq!(
            format_segments(&event, &state),
            vec![
                player_seg(&state, PlayerId(1)),
                text(" is dealt "),
                num(7),
                text(" combat damage by "),
                num(2),
                text(" creatures"),
            ]
        );
    }

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
            cast_mana_value: None,
        };
        let entries = resolve_log_entries(&[event], &state, &state);
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

    /// CR 309.4b-c: Most room entries are automatic, so the log is where a
    /// player learns which room they landed in and what it does. It must also
    /// survive the default timeline filter (`LogImportance::Context`).
    #[test]
    fn room_entered_log_names_the_room_and_its_effect() {
        use crate::game::dungeon::DungeonId;

        let state = GameState::new_two_player(42);
        let entries = resolve_log_entries(
            &[GameEvent::RoomEntered {
                player_id: PlayerId(0),
                dungeon: DungeonId::LostMineOfPhandelver,
                room_index: 2,
                room_name: "Mine Tunnels".to_string(),
            }],
            &state,
            &state,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].presentation.importance, LogImportance::Context);
        assert_eq!(
            entries[0].segments,
            vec![
                LogSegment::PlayerName {
                    name: "Player 1".to_string(),
                    player_id: PlayerId(0),
                },
                LogSegment::Text(" entered ".to_string()),
                LogSegment::Text("Mine Tunnels".to_string()),
                LogSegment::Text(" (".to_string()),
                LogSegment::Text("Lost Mine of Phandelver".to_string()),
                LogSegment::Text(")".to_string()),
                LogSegment::Text(": ".to_string()),
                LogSegment::Text("Create a Treasure token.".to_string()),
            ]
        );
    }

    #[test]
    fn completed_scry_has_a_public_count_only_log_entry() {
        let state = GameState::new_two_player(42);
        let entries = resolve_log_entries(
            &[GameEvent::PlayerPerformedAction {
                player_id: PlayerId(0),
                action: crate::types::events::PlayerActionKind::Scry,
                look_count: Some(3),
                scry_bottom_count: Some(2),
                scry_top_count: Some(1),
            }],
            &state,
            &state,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].presentation.visibility, LogVisibility::Public);
        assert_eq!(
            entries[0].segments,
            vec![
                LogSegment::PlayerName {
                    name: "Player 1".to_string(),
                    player_id: PlayerId(0),
                },
                LogSegment::Text(" scries ".to_string()),
                LogSegment::Number(3),
                LogSegment::Text(": ".to_string()),
                LogSegment::Number(1),
                LogSegment::Text(" on top and ".to_string()),
                LogSegment::Number(2),
                LogSegment::Text(" on bottom".to_string()),
            ]
        );
    }

    #[test]
    fn public_log_hides_hand_to_library_but_keeps_public_discard() {
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let mulligan = create_object(
            &mut state,
            CardId(98),
            PlayerId(1),
            "Secret Mulligan Card".to_string(),
            Zone::Library,
        );
        let discarded = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Public Discard".to_string(),
            Zone::Graveyard,
        );
        let mut mulligan_record =
            ZoneChangeRecord::test_minimal(mulligan, Some(Zone::Hand), Zone::Library);
        mulligan_record.name = "Secret Mulligan Card".to_string();
        let mut discard_record =
            ZoneChangeRecord::test_minimal(discarded, Some(Zone::Hand), Zone::Graveyard);
        discard_record.name = "Public Discard".to_string();
        let events = vec![
            GameEvent::ZoneChanged {
                object_id: mulligan,
                from: Some(Zone::Hand),
                to: Zone::Library,
                record: Box::new(mulligan_record),
            },
            GameEvent::ZoneChanged {
                object_id: discarded,
                from: Some(Zone::Hand),
                to: Zone::Graveyard,
                record: Box::new(discard_record),
            },
        ];

        let entries = resolve_log_entries(&events, &state, &state);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].segments.iter().any(
            |segment| matches!(segment, LogSegment::CardName { name, .. } if name == "Public Discard")
        ));
        assert!(entries.iter().all(|entry| entry.segments.iter().all(
            |segment| !matches!(segment, LogSegment::CardName { name, .. } if name == "Secret Mulligan Card")
        )));
    }

    #[test]
    fn public_log_hides_foretold_card_name_and_hand_to_exile_record() {
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let foretold = create_object(
            &mut state,
            CardId(704),
            PlayerId(1),
            "Secret Foretell".to_string(),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&foretold).unwrap();
        obj.foretold = true;
        obj.face_down = true;
        let mut record = ZoneChangeRecord::test_minimal(foretold, Some(Zone::Hand), Zone::Exile);
        record.name = "Secret Foretell".to_string();
        record.owner = PlayerId(1);
        let entries = resolve_log_entries(
            &[
                GameEvent::ZoneChanged {
                    object_id: foretold,
                    from: Some(Zone::Hand),
                    to: Zone::Exile,
                    record: Box::new(record),
                },
                GameEvent::Foretold {
                    player_id: PlayerId(1),
                    object_id: foretold,
                },
            ],
            &state,
            &state,
        );

        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].segments.as_slice(),
            [LogSegment::PlayerName { player_id, .. }, LogSegment::Text(text)]
                if *player_id == PlayerId(1) && text == " foretold a card"
        ));
    }

    #[test]
    fn draw_player_action_is_excluded_but_other_actions_are_logged() {
        use crate::types::events::PlayerActionKind;

        let state = GameState::new_two_player(42);
        // The Draw ledger signal must not reach the visible log —
        // this assertion flips (entries.len() == 1) if the exclusion is reverted.
        let draw_event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::Draw,
            look_count: None,
            scry_bottom_count: None,
            scry_top_count: None,
        };
        let draw_entries = resolve_log_entries(&[draw_event], &state, &state);
        assert!(
            draw_entries.is_empty(),
            "PlayerPerformedAction {{ Draw }} is a ledger-only signal and must be excluded from the log"
        );

        // Reach-guard against an over-broad exclusion: a non-Draw player action
        // (Scry) must still produce a log entry. Fails if someone excludes all
        // PlayerPerformedAction variants instead of just Draw.
        let scry_event = GameEvent::PlayerPerformedAction {
            player_id: PlayerId(0),
            action: PlayerActionKind::Scry,
            look_count: Some(1),
            scry_bottom_count: Some(0),
            scry_top_count: Some(1),
        };
        let scry_entries = resolve_log_entries(&[scry_event], &state, &state);
        assert_eq!(
            scry_entries.len(),
            1,
            "Non-Draw player actions must remain visible in the log"
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
    fn named_choice_guess_logs_as_debug_with_source() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gollum, Scheming Guide".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let event = GameEvent::CardPredicateGuessMade {
            player_id: PlayerId(1),
            source_id: Some(source_id),
            choice: "Nonland".to_string(),
        };
        let entries = resolve_log_entries(&[event], &state, &state);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, LogCategory::Debug);
        assert!(matches!(
            entries[0].segments.as_slice(),
            [
                LogSegment::PlayerName { player_id, .. },
                LogSegment::Text(guesses),
                LogSegment::Text(choice),
                LogSegment::Text(for_text),
                LogSegment::CardName { name, .. },
            ] if *player_id == PlayerId(1)
                && guesses == " guesses "
                && choice == "Nonland"
                && for_text == " for "
                && name == "Gollum, Scheming Guide"
        ));
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
                token_image_ref: None,
                power: Some(2),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
                tapped: false,
                is_suspected: false,
                attachments: Vec::new(),
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
                new_total: crate::types::events::LifeTotalReading::default(),
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
                new_total: crate::types::events::LifeTotalReading::default(),
            },
            &state,
        );
        assert!(segs
            .iter()
            .any(|s| matches!(s, LogSegment::Text(t) if t == " loses ")));
        assert!(segs.iter().any(|s| matches!(s, LogSegment::Number(3))));
    }

    #[test]
    fn source_aware_toxic_damage_replaces_its_life_loss_and_summary_lines() {
        let state = GameState::new_two_player(42);
        let entries = resolve_log_entries(
            &[
                GameEvent::LifeChanged {
                    player_id: PlayerId(1),
                    amount: -5,
                    new_total: crate::types::events::LifeTotalReading::default(),
                },
                GameEvent::ReplacementApplied {
                    source_id: ObjectId(9),
                    event_type: "AddPlayerCounter".to_string(),
                },
                GameEvent::PlayerCounterChanged {
                    player: PlayerId(1),
                    counter_kind: crate::types::player::PlayerCounterKind::Poison,
                    delta: 1,
                },
                GameEvent::DamageDealt {
                    source_id: ObjectId(7),
                    target: TargetRef::Player(PlayerId(1)),
                    amount: 5,
                    is_combat: true,
                    excess: 0,
                },
                GameEvent::CombatDamageDealtToPlayer {
                    player_id: PlayerId(1),
                    source_amounts: vec![(ObjectId(7), 5)],
                    total_damage: 5,
                },
            ],
            &state,
            &state,
        );

        assert_eq!(
            entries.len(),
            2,
            "keep the poison row and source-aware damage row"
        );
        assert!(entries
            .iter()
            .any(|entry| entry.category == LogCategory::Combat));
        assert!(entries
            .iter()
            .flat_map(|entry| &entry.segments)
            .any(|segment| matches!(segment, LogSegment::Text(text) if text == " deals ")));
        assert!(!entries
            .iter()
            .flat_map(|entry| &entry.segments)
            .any(|segment| matches!(segment, LogSegment::Text(text) if text == " loses ")));
    }

    #[test]
    fn an_earlier_identical_damage_row_does_not_hide_an_incomplete_later_summary() {
        let events = [
            GameEvent::DamageDealt {
                source_id: ObjectId(7),
                target: TargetRef::Player(PlayerId(1)),
                amount: 5,
                is_combat: true,
                excess: 0,
            },
            GameEvent::CombatDamageDealtToPlayer {
                player_id: PlayerId(1),
                source_amounts: vec![(ObjectId(7), 5)],
                total_damage: 5,
            },
            GameEvent::CombatDamageDealtToPlayer {
                player_id: PlayerId(1),
                source_amounts: vec![(ObjectId(7), 5)],
                total_damage: 5,
            },
        ];

        assert!(is_redundant_log_event(&events, 1));
        assert!(
            !is_redundant_log_event(&events, 2),
            "the first aggregate consumes its damage row; the later incomplete group remains visible"
        );
    }

    #[test]
    fn independent_life_loss_remains_visible() {
        let state = GameState::new_two_player(42);
        let entries = resolve_log_entries(
            &[GameEvent::LifeChanged {
                player_id: PlayerId(1),
                amount: -5,
                new_total: crate::types::events::LifeTotalReading::default(),
            }],
            &state,
            &state,
        );

        assert_eq!(entries.len(), 1);
        assert!(entries[0]
            .segments
            .iter()
            .any(|segment| matches!(segment, LogSegment::Text(text) if text == " loses ")));
    }

    #[test]
    fn equal_life_loss_from_an_earlier_effect_is_not_folded_into_damage() {
        let state = GameState::new_two_player(42);
        let entries = resolve_log_entries(
            &[
                GameEvent::LifeChanged {
                    player_id: PlayerId(1),
                    amount: -5,
                    new_total: crate::types::events::LifeTotalReading::default(),
                },
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::LoseLife,
                    source_id: ObjectId(8),
                    subject: None,
                },
                GameEvent::LifeChanged {
                    player_id: PlayerId(1),
                    amount: -5,
                    new_total: crate::types::events::LifeTotalReading::default(),
                },
                GameEvent::DamageDealt {
                    source_id: ObjectId(7),
                    target: TargetRef::Player(PlayerId(1)),
                    amount: 5,
                    is_combat: false,
                    excess: 0,
                },
            ],
            &state,
            &state,
        );

        assert_eq!(
            entries
                .iter()
                .flat_map(|entry| &entry.segments)
                .filter(|segment| matches!(segment, LogSegment::Text(text) if text == " loses "))
                .count(),
            1,
            "keep the independent life-loss row and hide only damage's derivative row"
        );
        assert!(entries
            .iter()
            .flat_map(|entry| &entry.segments)
            .any(|segment| matches!(segment, LogSegment::Text(text) if text == " deals ")));
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
                result: Some(17),
            },
            GameEvent::StartingPlayerContest {
                rounds: vec![crate::types::events::ContestRound {
                    rolls: vec![(PlayerId(0), 17), (PlayerId(1), 5)],
                }],
                winner: PlayerId(0),
            },
            GameEvent::CoinFlipped {
                player_id: PlayerId(0),
                won: true,
            },
            GameEvent::RingTemptsYou {
                player_id: PlayerId(0),
                chosen_bearer: None,
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
                source_id: ObjectId(0),
            },
            GameEvent::PowerToughnessChanged {
                object_id: ObjectId(1),
                power: 4,
                toughness: 5,
                power_delta: 2,
                toughness_delta: 2,
            },
        ];
        let entries = resolve_log_entries(&events, &state, &state);
        assert_eq!(entries.len(), events.len());
        for entry in &entries {
            assert!(
                !entry.segments.is_empty(),
                "Every event should produce at least one segment"
            );
        }
    }

    #[test]
    fn cursor_uses_pregame_context_then_turn_and_phase_boundaries() {
        let mut before = GameState::new_two_player(42);
        before.turn_number = 9;
        before.phase = Phase::End;
        let mut after = before.clone();
        after.turn_number = 1;
        after.phase = Phase::Upkeep;
        let entries = resolve_log_entries(
            &[
                GameEvent::StartingPlayerContest {
                    rounds: vec![],
                    winner: PlayerId(0),
                },
                GameEvent::GameStarted,
                GameEvent::TurnStarted {
                    player_id: PlayerId(0),
                    turn_number: 1,
                },
                GameEvent::PhaseChanged {
                    phase: Phase::Upkeep,
                },
                GameEvent::CardDrawn {
                    player_id: PlayerId(0),
                    object_id: ObjectId(77),
                    nth_in_turn: 1,
                    nth_in_step: 1,
                },
            ],
            &before,
            &after,
        );

        assert_eq!(entries[0].turn, 0);
        assert_eq!(entries[1].turn, 0);
        assert_eq!(entries[2].turn, 1);
        assert_eq!(entries[2].phase, Phase::Untap);
        assert_eq!(entries[3].phase, Phase::Upkeep);
        assert_eq!(entries[4].turn, 1);
        assert_eq!(
            entries[4].presentation.visibility,
            LogVisibility::HiddenInformation
        );
        assert!(
            matches!(entries[4].segments.as_slice(), [LogSegment::PlayerName { .. }, LogSegment::Text(text)] if text == " draws a card")
        );
    }

    #[test]
    fn factory_attaches_policy_metadata_and_omits_empty_entries() {
        let state = GameState::new_two_player(42);
        let entries = resolve_log_entries(
            &[
                GameEvent::LifeChanged {
                    player_id: PlayerId(0),
                    amount: 3,
                    new_total: crate::types::events::LifeTotalReading::default(),
                },
                GameEvent::LifeChanged {
                    player_id: PlayerId(1),
                    amount: -3,
                    new_total: crate::types::events::LifeTotalReading::default(),
                },
                GameEvent::TappedForMana {
                    source_id: ObjectId(1),
                    player_id: PlayerId(0),
                    produced: vec![],
                    tap_state: Default::default(),
                },
            ],
            &state,
            &state,
        );

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].presentation.importance, LogImportance::Essential);
        assert_eq!(entries[0].presentation.tone, LogTone::Positive);
        assert_eq!(entries[1].presentation.tone, LogTone::Negative);
    }

    #[test]
    fn presentation_policy_table_covers_importance_and_polarity() {
        let cases = [
            (
                GameEvent::GameStarted,
                LogImportance::Essential,
                LogTone::Neutral,
            ),
            (
                GameEvent::PhaseChanged {
                    phase: Phase::Upkeep,
                },
                LogImportance::Context,
                LogTone::Neutral,
            ),
            (
                GameEvent::LifeChanged {
                    player_id: PlayerId(0),
                    amount: 1,
                    new_total: crate::types::events::LifeTotalReading::default(),
                },
                LogImportance::Essential,
                LogTone::Positive,
            ),
            (
                GameEvent::DamageDealt {
                    source_id: ObjectId(1),
                    target: TargetRef::Player(PlayerId(1)),
                    amount: 2,
                    is_combat: false,
                    excess: 0,
                },
                LogImportance::Essential,
                LogTone::Negative,
            ),
            (
                GameEvent::DebugActionUsed {
                    player_id: PlayerId(0),
                    description: "set life".to_string(),
                },
                LogImportance::Diagnostic,
                LogTone::Diagnostic,
            ),
            (
                GameEvent::TappedForMana {
                    source_id: ObjectId(1),
                    player_id: PlayerId(0),
                    produced: vec![],
                    tap_state: Default::default(),
                },
                LogImportance::Detail,
                LogTone::Neutral,
            ),
        ];

        for (event, expected_importance, expected_tone) in cases {
            assert_eq!(importance(&event), expected_importance, "{event:?}");
            assert_eq!(tone(&event), expected_tone, "{event:?}");
        }
    }

    #[test]
    fn start_game_log_entries_reset_hostile_context_before_turn_one() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 99;
        state.phase = Phase::End;

        let result = start_game(&mut state);

        assert!(matches!(
            result.events.as_slice(),
            [
                GameEvent::StartingPlayerContest { .. },
                GameEvent::GameStarted,
                GameEvent::TurnStarted { turn_number: 1, .. },
                ..
            ]
        ));
        assert_eq!(result.log_entries[0].turn, 0);
        assert_eq!(result.log_entries[0].phase, Phase::Untap);
        assert_eq!(result.log_entries[1].turn, 0);
        assert_eq!(result.log_entries[1].phase, Phase::Untap);
        assert_eq!(result.log_entries[2].turn, 1);
        assert_eq!(result.log_entries[2].phase, Phase::Untap);
    }

    #[test]
    fn explicit_and_skip_mulligan_starts_reset_context_without_a_contest() {
        let mut explicit_state = GameState::new_two_player(42);
        explicit_state.turn_number = 99;
        explicit_state.phase = Phase::End;
        let explicit = start_game_with_starting_player(&mut explicit_state, PlayerId(1));

        let mut skip_state = GameState::new_two_player(42);
        skip_state.turn_number = 99;
        skip_state.phase = Phase::End;
        let skipped = start_game_skip_mulligan(&mut skip_state);

        for result in [&explicit, &skipped] {
            assert!(!result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::StartingPlayerContest { .. })));
            assert!(matches!(
                result.events.as_slice(),
                [
                    GameEvent::GameStarted,
                    GameEvent::TurnStarted { turn_number: 1, .. },
                    ..
                ]
            ));
            assert_eq!(result.log_entries[0].turn, 0);
            assert_eq!(result.log_entries[0].phase, Phase::Untap);
            assert_eq!(result.log_entries[1].turn, 1);
            assert_eq!(result.log_entries[1].phase, Phase::Untap);
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
            presentation: Default::default(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: GameLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn legacy_log_json_defaults_presentation() {
        let json = r#"{"seq":1,"turn":1,"phase":"Untap","category":"Game","segments":[{"type":"Text","value":"Game started"}]}"#;
        let entry: GameLogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.presentation, LogPresentation::default());
        let serialized = serde_json::to_value(&entry).unwrap();
        assert_eq!(serialized["presentation"]["importance"], "Detail");
    }

    fn zone_move(
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        owner: PlayerId,
        turn_zone_change_index: usize,
    ) -> GameEvent {
        let mut record =
            crate::types::game_state::ZoneChangeRecord::test_minimal(object_id, Some(from), to);
        record.owner = owner;
        record.turn_zone_change_index = turn_zone_change_index;
        GameEvent::ZoneChanged {
            object_id,
            from: Some(from),
            to,
            record: Box::new(record),
        }
    }

    fn journal_zone_move(state: &mut GameState, event: &GameEvent, cause: RulesExecutionNodeRef) {
        let GameEvent::ZoneChanged {
            object_id,
            from: Some(from),
            to,
            record,
        } = event
        else {
            panic!("journal_zone_move takes a ZoneChanged with an origin");
        };
        state
            .resolved_rules_journal
            .record_zone_change(crate::types::resolved_commands::ResolvedZoneChangeCommand {
                object: crate::types::identifiers::ObjectIncarnationRef::of(*object_id, 0),
                resulting_incarnation: 1,
                from: *from,
                to: *to,
                destination_position: 0,
                owner: record.owner,
                entry_timestamp: None,
                turn_zone_change_index: record.turn_zone_change_index,
                zone_change_record: (**record).clone(),
                cause,
            })
            .unwrap();
    }

    fn is_first_a_leave_move(events: &[GameEvent], state: &GameState) -> bool {
        is_player_leave_move(events, 0, &BatchIndex::new(events, state))
    }

    /// CR 800.4a: only the exact move the leave node performed is the sweep.
    #[test]
    fn player_leave_journal_key_scopes_the_hidden_card_exclusion() {
        let mut state = GameState::new_two_player(42);
        let proposal = state.resolved_rules_journal.begin_proposal().unwrap();
        let leave = state.resolved_rules_journal.begin_player_leave().unwrap();
        let (kept, graveyard_card, unjournaled) = (ObjectId(7), ObjectId(8), ObjectId(9));
        let face_up_exile = zone_move(kept, Zone::Hand, Zone::Exile, PlayerId(1), 0);
        let sweep_exile = zone_move(kept, Zone::Hand, Zone::Exile, PlayerId(1), 2);
        let public_sweep = zone_move(graveyard_card, Zone::Graveyard, Zone::Exile, PlayerId(1), 1);
        let no_command = zone_move(unjournaled, Zone::Hand, Zone::Exile, PlayerId(1), 3);
        journal_zone_move(&mut state, &face_up_exile, proposal);
        journal_zone_move(&mut state, &sweep_exile, leave);
        journal_zone_move(&mut state, &public_sweep, leave);

        assert!(is_first_a_leave_move(&[sweep_exile], &state));
        assert!(!is_first_a_leave_move(&[face_up_exile], &state));
        assert!(!is_first_a_leave_move(&[public_sweep], &state));
        assert!(!is_first_a_leave_move(&[no_command], &state));
    }

    /// CR 800.4a: across a turn start the batch must show the owner's elimination before it.
    #[test]
    fn turn_crossing_player_leave_fallback_boundaries() {
        let state = GameState::new_two_player(42);
        let (leaver, other) = (PlayerId(1), PlayerId(0));
        let hand_exile = |owner| zone_move(ObjectId(7), Zone::Hand, Zone::Exile, owner, 0);
        let eliminated = GameEvent::PlayerEliminated { player_id: leaver };
        let turn_started = GameEvent::TurnStarted {
            player_id: other,
            turn_number: 3,
        };

        let crossed = [hand_exile(leaver), eliminated.clone(), turn_started.clone()];
        assert!(is_first_a_leave_move(&crossed, &state));

        let eliminated_after_turn_start =
            [hand_exile(leaver), turn_started.clone(), eliminated.clone()];
        let to_graveyard = [
            zone_move(ObjectId(7), Zone::Hand, Zone::Graveyard, leaver, 0),
            eliminated.clone(),
            turn_started.clone(),
        ];
        let other_owner = [hand_exile(other), eliminated.clone(), turn_started.clone()];
        let same_turn = [hand_exile(leaver), eliminated.clone()];
        let public_origin = [
            zone_move(ObjectId(8), Zone::Graveyard, Zone::Exile, other, 0),
            GameEvent::PlayerEliminated { player_id: other },
            turn_started,
        ];
        for batch in [
            &eliminated_after_turn_start[..],
            &to_graveyard,
            &other_owner,
            &same_turn,
            &public_origin,
        ] {
            assert!(!is_first_a_leave_move(batch, &state), "{batch:?}");
        }
    }

    #[test]
    fn same_zone_move_is_not_narrated() {
        let exile_to_exile = zone_move(ObjectId(7), Zone::Exile, Zone::Exile, PlayerId(1), 0);
        let graveyard_to_exile =
            zone_move(ObjectId(7), Zone::Graveyard, Zone::Exile, PlayerId(1), 0);
        assert!(should_exclude_event(&exile_to_exile));
        assert!(!should_exclude_event(&graveyard_to_exile));
    }

    #[test]
    fn event_time_name_reads_public_origin_records_only() {
        let state = GameState::new_two_player(42);
        let card = ObjectId(7);
        let named_move = |from, to, name: &str| {
            let GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                mut record,
            } = zone_move(card, from, to, PlayerId(0), 0)
            else {
                unreachable!()
            };
            record.name = name.to_string();
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                record,
            }
        };
        let rename = |later: &[GameEvent]| {
            let mut segments = vec![LogSegment::CardName {
                name: "After Name".to_string(),
                object_id: card,
            }];
            name_at_event_time(&mut segments, &BatchIndex::new(later, &state), 0);
            match &segments[0] {
                LogSegment::CardName { name, .. } => name.clone(),
                other => panic!("{other:?}"),
            }
        };

        assert_eq!(
            rename(&[named_move(Zone::Stack, Zone::Exile, "Stack Name")]),
            "Stack Name"
        );
        assert_eq!(
            rename(&[
                named_move(Zone::Stack, Zone::Graveyard, "First Name"),
                named_move(Zone::Graveyard, Zone::Exile, "Second Name"),
            ]),
            "First Name"
        );
        assert_eq!(
            rename(&[
                named_move(Zone::Hand, Zone::Stack, "Hidden Name"),
                named_move(Zone::Stack, Zone::Exile, "Stack Name"),
            ]),
            "After Name"
        );
        let other_card = zone_move(ObjectId(8), Zone::Stack, Zone::Exile, PlayerId(0), 0);
        assert_eq!(rename(&[other_card]), "After Name");

        let mut scratch = GameState::new_two_player(42);
        let hidden = create_object(
            &mut scratch,
            CardId(1),
            PlayerId(0),
            "Hidden Name".to_string(),
            Zone::Exile,
        );
        scratch.objects.get_mut(&hidden).unwrap().face_down = true;
        let face_down_departure = GameEvent::ZoneChanged {
            object_id: card,
            from: Some(Zone::Exile),
            to: Zone::Hand,
            record: Box::new(scratch.objects[&hidden].snapshot_for_zone_change(
                card,
                Some(Zone::Exile),
                Zone::Hand,
            )),
        };
        assert_eq!(rename(&[face_down_departure]), "");
    }

    fn snapshot_move(state: &GameState, object_id: ObjectId, from: Zone, to: Zone) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(from),
            to,
            record: Box::new(state.objects[&object_id].snapshot_for_zone_change(
                object_id,
                Some(from),
                to,
            )),
        }
    }

    fn set_face_down(state: &mut GameState, object_id: ObjectId, face_down: bool) {
        state.objects.get_mut(&object_id).unwrap().face_down = face_down;
    }

    fn has_move_line(entries: &[GameLogEntry], id: ObjectId, from: Zone, to: Zone) -> bool {
        entries.iter().any(|entry| {
            matches!(
                entry.segments.as_slice(),
                [
                    LogSegment::CardName { object_id, .. },
                    LogSegment::Text(_),
                    LogSegment::Zone(logged_from),
                    LogSegment::Text(_),
                    LogSegment::Zone(logged_to),
                ] if *object_id == id && *logged_from == from && *logged_to == to
            )
        })
    }

    fn naming_count(entries: &[GameLogEntry], id: ObjectId) -> usize {
        entries
            .iter()
            .filter(|entry| {
                entry.segments.iter().any(|segment| {
                    matches!(segment, LogSegment::CardName { object_id, .. } if *object_id == id)
                })
            })
            .count()
    }

    /// CR 406.3: a card exiled face down and moved on to a hidden zone in one batch never showed
    /// its face.
    #[test]
    fn face_down_round_trip_in_one_batch_is_unnamed() {
        let mut state = GameState::new_two_player(42);
        let hidden = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Probe Round Trip".to_string(),
            Zone::Library,
        );
        let milled = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Probe Milled".to_string(),
            Zone::Library,
        );
        let to_exile = snapshot_move(&state, hidden, Zone::Library, Zone::Exile);
        let mill = snapshot_move(&state, milled, Zone::Library, Zone::Graveyard);
        set_face_down(&mut state, hidden, true);
        let to_hand = snapshot_move(&state, hidden, Zone::Exile, Zone::Hand);
        set_face_down(&mut state, hidden, false);

        let entries = resolve_log_entries(&[to_exile, mill, to_hand], &state, &state);
        assert!(
            has_move_line(&entries, milled, Zone::Library, Zone::Graveyard),
            "{entries:?}"
        );
        assert_eq!(naming_count(&entries, hidden), 0, "{entries:?}");
    }

    /// CR 406.3 + CR 708.9: a face-down arrival is not narrated, but leaving the battlefield
    /// reveals the card.
    #[test]
    fn face_down_arrival_line_is_dropped_when_it_dies_in_the_batch() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Probe Manifest".to_string(),
            Zone::Library,
        );
        let arrive = snapshot_move(&state, card, Zone::Library, Zone::Battlefield);
        set_face_down(&mut state, card, true);
        state.objects.get_mut(&card).unwrap().name = String::new();
        let dies = snapshot_move(&state, card, Zone::Battlefield, Zone::Graveyard);
        set_face_down(&mut state, card, false);
        state.objects.get_mut(&card).unwrap().name = "Probe Manifest".to_string();

        let entries = resolve_log_entries(&[arrive, dies], &state, &state);
        assert!(
            has_move_line(&entries, card, Zone::Battlefield, Zone::Graveyard),
            "{entries:?}"
        );
        assert!(
            !has_move_line(&entries, card, Zone::Library, Zone::Battlefield),
            "{entries:?}"
        );
    }

    /// CR 708.9: a face-down spell is revealed only when it leaves the stack for a zone other
    /// than the battlefield.
    #[test]
    fn face_down_spell_resolving_to_the_battlefield_is_unnarrated() {
        let mut state = GameState::new_two_player(42);
        let resolved = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Probe Resolved Morph".to_string(),
            Zone::Stack,
        );
        let countered = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Probe Countered Morph".to_string(),
            Zone::Stack,
        );
        set_face_down(&mut state, resolved, true);
        set_face_down(&mut state, countered, true);
        let resolve = snapshot_move(&state, resolved, Zone::Stack, Zone::Battlefield);
        let counter = snapshot_move(&state, countered, Zone::Stack, Zone::Graveyard);
        set_face_down(&mut state, countered, false);

        let entries = resolve_log_entries(&[resolve, counter], &state, &state);
        assert!(
            has_move_line(&entries, countered, Zone::Stack, Zone::Graveyard),
            "{entries:?}"
        );
        assert!(
            !has_move_line(&entries, resolved, Zone::Stack, Zone::Battlefield),
            "{entries:?}"
        );
    }

    /// CR 708.9: a face-down permanent is revealed as it leaves the battlefield.
    #[test]
    fn face_down_permanent_bounce_stays_named() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Probe Morph".to_string(),
            Zone::Battlefield,
        );
        set_face_down(&mut state, card, true);
        let bounce = snapshot_move(&state, card, Zone::Battlefield, Zone::Hand);
        set_face_down(&mut state, card, false);

        let entries = resolve_log_entries(&[bounce], &state, &state);
        assert!(
            has_move_line(&entries, card, Zone::Battlefield, Zone::Hand),
            "{entries:?}"
        );
    }

    #[test]
    fn context_free_exile_departure_stays_named() {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Probe Legacy".to_string(),
            Zone::Hand,
        );
        let entries = resolve_log_entries(
            &[zone_move(card, Zone::Exile, Zone::Hand, PlayerId(0), 0)],
            &state,
            &state,
        );
        assert!(
            has_move_line(&entries, card, Zone::Exile, Zone::Hand),
            "{entries:?}"
        );
    }

    #[test]
    fn batch_resolution_scales_near_linearly() {
        let mut state = GameState::new_two_player(42);
        let proposal = state.resolved_rules_journal.begin_proposal().unwrap();
        let events: Vec<GameEvent> = (0..40_000u64)
            .flat_map(|i| {
                [
                    GameEvent::KeywordAbilityActivated {
                        ability_tag: AbilityTag::Equip,
                        player_id: PlayerId(0),
                        source_id: ObjectId(i),
                        is_mana_ability: false,
                    },
                    zone_move(
                        ObjectId(100_000 + i),
                        Zone::Library,
                        Zone::Graveyard,
                        PlayerId(0),
                        i as usize,
                    ),
                ]
            })
            .collect();
        for event in events.iter().skip(1).step_by(2) {
            journal_zone_move(&mut state, event, proposal);
        }

        let started = std::time::Instant::now();
        let entries = resolve_log_entries(&events, &state, &state);
        let elapsed = started.elapsed();

        assert_eq!(entries.len(), events.len());
        assert!(
            elapsed < std::time::Duration::from_secs(8),
            "80k-event batch took {elapsed:?}, limit 8s"
        );
    }

    #[test]
    fn untagged_or_unpaired_activation_keeps_its_line() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Probe Equipment".to_string(),
            Zone::Battlefield,
        );
        let other_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Probe Other".to_string(),
            Zone::Battlefield,
        );
        let generic = |source_id| GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id,
            kind: Default::default(),
        };
        let keyword = |source_id| GameEvent::KeywordAbilityActivated {
            ability_tag: AbilityTag::Equip,
            player_id: PlayerId(0),
            source_id,
            is_mana_ability: false,
        };
        let lines = |events: &[GameEvent]| resolve_log_entries(events, &state, &state).len();

        assert_eq!(lines(&[generic(source), keyword(source)]), 1);
        assert_eq!(lines(&[generic(source), keyword(other_source)]), 2);
        assert_eq!(lines(&[keyword(source)]), 1);
        assert_eq!(lines(&[generic(source)]), 1);
    }
}

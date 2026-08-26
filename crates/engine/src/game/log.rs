use crate::types::ability::{AbilityTag, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::log::{
    GameLogEntry, LogBoundary, LogCategory, LogImportance, LogPresentation, LogSegment, LogTone,
    LogVisibility,
};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

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

    events
        .iter()
        .filter_map(|event| {
            cursor.apply(event);
            (!should_exclude_event(event, after)).then(|| {
                let segments = format_segments(event, after);
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
        // The remaining variants are deliberately listed rather than covered by a
        // wildcard. Adding a GameEvent must require an explicit presentation policy.
        GameEvent::HiddenSearchViewed { .. }
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
        | GameEvent::DamagePrevented { .. }
        | GameEvent::SpellCountered { .. }
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
        GameEvent::LifeChanged { .. }
        | GameEvent::GameStarted
        | GameEvent::HiddenSearchViewed { .. }
        | GameEvent::CreatureExploited { .. }
        | GameEvent::TurnStarted { .. }
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
fn should_exclude_event(event: &GameEvent, state: &GameState) -> bool {
    match event {
        GameEvent::HiddenSearchViewed { .. } => true,
        // Library-origin moves and mulligan/tuck moves from hand to library
        // expose hidden card identity. Public discard/moves remain loggable.
        GameEvent::ZoneChanged {
            from: Some(crate::types::zones::Zone::Library),
            ..
        }
        | GameEvent::ZoneChanged {
            from: Some(crate::types::zones::Zone::Hand),
            to: crate::types::zones::Zone::Library,
            ..
        } => true,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(crate::types::zones::Zone::Hand),
            to: crate::types::zones::Zone::Exile,
            ..
        } if state
            .objects
            .get(object_id)
            .is_some_and(|obj| obj.face_down) =>
        {
            true
        }
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
        // CR 714.2: the chapter-resolution notification exists so meta-triggers
        // can observe it; the player already saw the chapter ability itself
        // resolve. Same low-signal bookkeeping class as StackResolved.
        GameEvent::SagaChapterAbilityResolved { .. } => true,
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
        | GameEvent::ManaRecolored { .. } => LogCategory::Mana,

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
            text(" performed action "),
            text(&format!("{action:?}")),
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
            ..
        } => vec![
            player_seg(state, *player_id),
            text(" is dealt combat damage by "),
            num(source_amounts.len() as i32),
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
        // CR 500.5 + CR 703.4q: A unit was emptied from a pool at step end.
        GameEvent::ManaPoolEmptied {
            player_id, color, ..
        } => vec![
            player_seg(state, *player_id),
            text(" loses "),
            LogSegment::Mana(format!("{color:?}")),
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
            LogSegment::Mana(format!("{from:?}")),
            text(" mana becomes "),
            LogSegment::Mana(format!("{to:?}")),
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
            LogSegment::Keyword(format!("{counter_type:?}")),
            text(" counter(s) on "),
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
            LogSegment::Keyword(format!("{counter_type:?}")),
            text(" counter(s) removed from "),
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
                text(&format!(" specializes ({color:?})")),
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

        GameEvent::EffectResolved {
            kind, source_id, ..
        } => vec![
            card_seg(state, *source_id),
            text(": "),
            text(&format!("{kind:?}")),
        ],

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
            vec![text(&format!("Rolled the planar die: {face:?}"))]
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
            text(&format!("{kind:?}").to_lowercase()),
            text(" sticker on "),
            card_seg(state, *object_id),
        ],
        GameEvent::AttractionsRolledToVisit { roll, .. } => {
            vec![
                text("Rolled "),
                text(&roll.to_string()),
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
                },
                GameEvent::LifeChanged {
                    player_id: PlayerId(1),
                    amount: -3,
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
}

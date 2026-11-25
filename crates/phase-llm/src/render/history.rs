//! Turn-cycle-compacted game history for an LLM prompt.
//!
//! A prompt is rebuilt for every decision, so whatever history it carries is
//! paid for again on every priority pass. Sending the raw log makes that cost
//! grow with the game; this module bounds it instead.
//!
//! The history is cut at turn-cycle boundaries — a cycle is one turn for every
//! seat still in the game. The cycle in progress is rendered entry by entry,
//! because what just happened is what the decision in front of the seat is
//! about. Each COMPLETED cycle collapses to one line per turn carrying only the
//! events the engine marks `LogImportance::Essential`, and only the most recent
//! completed cycles are kept at all. Nothing older is lost from the position:
//! its consequences are the board, the graveyards, the exile zone and the life
//! totals that [`super::game::render_board`] prints from live state on every
//! decision. The summary carries what the board cannot — who did what, and in
//! which order.
//!
//! What gets kept is decided by the engine's own presentation metadata
//! (`LogImportance`, `LogBoundary`), never by matching log text, so a new event
//! kind is summarized correctly the day the engine classifies it.
//!
//! Every player is named by seat, in the position's words
//! ([`super::game::seat_label`]): `You` for the seat the prompt belongs to,
//! `Player N` for every other. The engine's log writes a name for the on-screen
//! log, a display name or `Player N` counted from 1, and records beside it the
//! seat it means. The seat is what this module reads, so the history calls each
//! player what the position's seat labels call them.

use std::collections::BTreeMap;

use engine::game::players;
use engine::types::game_state::GameState;
use engine::types::log::{
    GameLogEntry, LogBoundary, LogCategory, LogImportance, LogSegment, LogVisibility,
};
use engine::types::player::PlayerId;

use super::game::seat_label;
use super::text::{clamp_text, one_line};

/// How much history a prompt carries. Driven by difficulty (see
/// [`crate::prompt::history_budget`]) so a low-difficulty seat genuinely
/// reasons from a shorter memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryBudget {
    /// Most recent COMPLETED turn cycles kept as one-line-per-turn summaries.
    /// `0` keeps none.
    pub summarized_cycles: usize,
    /// Most recent entries of the turn cycle in progress, rendered entry by
    /// entry. `0` omits the whole history section, summaries included. A
    /// message-thread continue shows as many of the entries since the seat's
    /// last decision entry by entry ([`push_since_last_decision`]).
    pub current_cycle_entries: usize,
}

impl HistoryBudget {
    /// No history at all.
    pub const NONE: HistoryBudget = HistoryBudget {
        summarized_cycles: 0,
        current_cycle_entries: 0,
    };
}

/// Characters kept for one summarized turn. A turn of a long combo or a
/// go-wide combat can log dozens of essential events; one line per turn is the
/// promise, so the line is clamped rather than allowed to sprawl.
const TURN_SUMMARY_BUDGET: usize = 360;

/// How a game's turns group into cycles.
///
/// CR 102.1 + CR 103.1: a turn belongs to its active player, and turns proceed
/// around the table in turn order, so one turn per living seat is the smallest
/// span in which every player has acted. The engine's `turn_number` counts
/// every turn (extra turns included), so cycles are fixed-width windows over it. An extra turn or an
/// elimination shifts where later windows fall; that changes only how history
/// is grouped, never what the position says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnCycle {
    pub turns_per_cycle: u32,
    pub current_turn: u32,
}

impl TurnCycle {
    /// The cycle shape of `state`, from the engine's own liveness authority.
    pub fn of(state: &GameState) -> TurnCycle {
        let living = state
            .players
            .iter()
            .filter(|player| players::is_alive(state, player.id))
            .count();
        TurnCycle {
            turns_per_cycle: u32::try_from(living).unwrap_or(u32::MAX).max(1),
            current_turn: state.turn_number,
        }
    }

    /// Which cycle `turn` falls in. Pre-game entries (turn 0) share the first
    /// cycle with turn 1.
    fn index_of(&self, turn: u32) -> u32 {
        turn.saturating_sub(1) / self.turns_per_cycle
    }

    fn current_index(&self) -> u32 {
        self.index_of(self.current_turn)
    }
}

/// Render the history section into `out`, naming players as `viewer`'s
/// position does ([`render_log_entry`]).
///
/// `history` is one game's entries, oldest first. Turn numbers restart with
/// every game of a match, so entries of another game would land in this
/// game's turn cycles; the prompt builder hands over only the game its
/// position belongs to (`build_game_decision_prompt`).
pub fn push_history<'a>(
    out: &mut String,
    history: impl IntoIterator<Item = &'a GameLogEntry>,
    viewer: PlayerId,
    cycle: TurnCycle,
    budget: HistoryBudget,
) {
    if budget.current_cycle_entries == 0 {
        return;
    }
    // Filter BEFORE windowing so dropped entries do not consume the budget —
    // otherwise a burst of draws would silently shorten the visible history.
    let visible: Vec<&GameLogEntry> = history
        .into_iter()
        .filter(|entry| is_prompt_safe(entry))
        .collect();
    if visible.is_empty() {
        return;
    }

    let current = cycle.current_index();
    let (completed, in_progress): (Vec<&GameLogEntry>, Vec<&GameLogEntry>) = visible
        .into_iter()
        .partition(|entry| cycle.index_of(entry.turn) < current);

    push_cycle_summaries(out, &completed, viewer, cycle, budget.summarized_cycles);
    push_current_cycle(out, &in_progress, viewer, budget.current_cycle_entries);
}

/// One line per turn of the most recent completed cycles.
fn push_cycle_summaries(
    out: &mut String,
    completed: &[&GameLogEntry],
    viewer: PlayerId,
    cycle: TurnCycle,
    cycles_kept: usize,
) {
    if cycles_kept == 0 {
        return;
    }
    let newest_cycle = cycle.current_index().saturating_sub(1);
    let oldest_kept = newest_cycle.saturating_sub(u32::try_from(cycles_kept - 1).unwrap_or(0));
    let kept: Vec<&GameLogEntry> = completed
        .iter()
        .copied()
        .filter(|entry| cycle.index_of(entry.turn) >= oldest_kept)
        .collect();
    if kept.is_empty() {
        return;
    }
    out.push_str("\n--- EARLIER TURNS (key events, one line per turn) ---\n");
    push_turn_summaries(out, &kept, viewer, None);
}

/// One line per turn of `entries` ([`summarize_turn`]).
///
/// `BTreeMap` keyed by turn so turns render in game order however the log
/// was interleaved, and the same history always renders identically.
/// `continued` is a turn whose newer entries follow entry by entry
/// ([`push_since_last_decision`]): its line covers only the entries here.
fn push_turn_summaries(
    out: &mut String,
    entries: &[&GameLogEntry],
    viewer: PlayerId,
    continued: Option<u32>,
) {
    let mut turns: BTreeMap<u32, Vec<&GameLogEntry>> = BTreeMap::new();
    for entry in entries {
        turns.entry(entry.turn).or_default().push(entry);
    }
    for (turn, turn_entries) in turns {
        let part = if continued == Some(turn) {
            TurnPart::Opening
        } else {
            TurnPart::Whole
        };
        out.push_str(&summarize_turn(turn, &turn_entries, viewer, part));
        out.push('\n');
    }
}

/// How much of its turn a summary line covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnPart {
    /// Every entry of the turn the section holds; none follows entry by
    /// entry.
    Whole,
    /// The turn's older entries, whose newer ones follow entry by entry.
    Opening,
}

/// `T5 (Turn 5 — Player 1): Player 1 plays Mountain; Player 1 casts Shock`,
/// or for a turn's opening `T5 (Turn 5 — Player 1, continued below): …`.
fn summarize_turn(
    turn: u32,
    entries: &[&GameLogEntry],
    viewer: PlayerId,
    part: TurnPart,
) -> String {
    let header = entries
        .iter()
        .find(|entry| entry.presentation.boundary == LogBoundary::Turn)
        .map_or_else(
            || format!("Turn {turn}"),
            |entry| render_log_entry(entry, viewer),
        );
    let events: Vec<String> = entries
        .iter()
        .filter(|entry| {
            entry.presentation.boundary == LogBoundary::None
                && entry.presentation.importance == LogImportance::Essential
        })
        .map(|entry| render_log_entry(entry, viewer))
        .filter(|text| !text.is_empty())
        .collect();
    // An opening is marked in the header, ahead of the events a clamp cuts
    // from the end, and with no key events it does not call the turn quiet:
    // its newer entries follow.
    let (header, quiet) = match part {
        TurnPart::Whole => (header, "nothing notable"),
        TurnPart::Opening => (format!("{header}, continued below"), "nothing notable yet"),
    };
    let body = if events.is_empty() {
        quiet.to_string()
    } else {
        events.join("; ")
    };
    clamp_text(&format!("T{turn} ({header}): {body}"), TURN_SUMMARY_BUDGET)
}

/// The cycle in progress, entry by entry: the events a player's default
/// timeline shows ([`shows_in_timeline`]).
fn push_current_cycle(out: &mut String, entries: &[&GameLogEntry], viewer: PlayerId, limit: usize) {
    let shown: Vec<&GameLogEntry> = entries
        .iter()
        .copied()
        .filter(|entry| shows_in_timeline(entry))
        .collect();
    if shown.is_empty() {
        return;
    }
    out.push_str("\n--- THIS TURN CYCLE (oldest first) ---\n");
    push_timeline_lines(out, &shown, viewer, limit);
}

/// What the log recorded after the entry numbered `since`, or the whole log
/// when `None`, that a message-thread continue carries, oldest first.
///
/// The same entries the turn cycle in progress shows: the same prompt-safety
/// filter ([`is_prompt_safe`]) and the same timeline entries
/// ([`shows_in_timeline`]). A continue shows the newest of them entry by entry
/// and folds any older ones into one line of key events per turn
/// ([`push_since_last_decision`]).
pub fn unseen_timeline_entries(history: &[GameLogEntry], since: Option<u32>) -> Vec<&GameLogEntry> {
    history
        .iter()
        .filter(|entry| since.is_none_or(|seen| entry.seq > seen))
        .filter(|entry| is_prompt_safe(entry) && shows_in_timeline(entry))
        .collect()
}

/// Render `unseen` ([`unseen_timeline_entries`]) as the opening section of a
/// message-thread continue.
///
/// The thread already holds everything up to the seat's last answer, so this
/// is the only history a continue carries. The newest `limit` entries are
/// rendered entry by entry, in the line format of the turn cycle in progress.
/// Any older ones, which a busy round at a multiplayer table logs before the
/// seat decides again, are folded into one line of key events per turn, as
/// completed cycles are ([`push_turn_summaries`]): their context events are
/// not carried, and a long turn's line is clamped. A turn the split falls
/// inside gets a line for its older entries, marked as continued, and its
/// newer entries below. `limit` is the seat's history window, so a continue
/// shows no more entry by entry than its full request would. Players are named
/// as `viewer`'s position names them ([`render_log_entry`]).
pub fn push_since_last_decision(
    out: &mut String,
    unseen: &[&GameLogEntry],
    viewer: PlayerId,
    limit: usize,
) {
    if unseen.is_empty() {
        out.push_str("--- SINCE YOUR LAST DECISION (oldest first) ---\n");
        out.push_str("Nothing new has been logged.\n");
        return;
    }
    let (earlier, latest) = unseen.split_at(unseen.len().saturating_sub(limit));
    if !earlier.is_empty() {
        out.push_str("--- SINCE YOUR LAST DECISION, EARLIER (key events, one line per turn) ---\n");
        let continued = latest.first().map(|entry| entry.turn);
        push_turn_summaries(out, earlier, viewer, continued);
    }
    if !latest.is_empty() {
        out.push_str("--- SINCE YOUR LAST DECISION (oldest first) ---\n");
        push_timeline_lines(out, latest, viewer, latest.len());
    }
}

/// Whether an entry is one a player's default timeline shows: `Essential` and
/// `Context` events. Phase boundaries are dropped — every line already names
/// its phase.
fn shows_in_timeline(entry: &GameLogEntry) -> bool {
    matches!(
        entry.presentation.importance,
        LogImportance::Essential | LogImportance::Context
    ) && entry.presentation.boundary != LogBoundary::Phase
}

/// `T5 PreCombatMain: Player 1 casts Shock`, one line for each of the newest
/// `limit` entries, oldest first.
fn push_timeline_lines(out: &mut String, shown: &[&GameLogEntry], viewer: PlayerId, limit: usize) {
    let start = shown.len().saturating_sub(limit);
    for entry in &shown[start..] {
        out.push_str(&format!(
            "T{} {:?}: {}\n",
            entry.turn,
            entry.phase,
            render_log_entry(entry, viewer)
        ));
    }
}

/// Whether a log entry may appear in a prompt.
///
/// Two independent exclusions, for two different reasons.
///
/// `LogVisibility::HiddenInformation` is not a display hint: it marks entries
/// the normal game log must not disclose — card draws name the exact card via
/// `LogSegment::CardName` (`engine::game::log::visibility`). A prompt leaves the
/// machine for a third-party provider, a strictly weaker boundary than the
/// on-screen log that classification was written for, so the same bar applies.
///
/// `LogCategory::Debug` is excluded because it is not a record of the GAME at
/// all — it is a diagnostic channel the client writes into, and its text can
/// originate outside this process. A provider's error detail travels as
/// `LlmError::Provider { detail }`, and a provider, a proxy, or a hostile custom
/// endpoint controls that string. Were a diagnostic entry renderable, such a
/// string could be written into the log and then read back to the model as
/// ordinary history on the next decision — prose that looks like history but is
/// authored by the very party the response validation exists to distrust.
/// Response validation does not help here: the text never has to pass as a
/// decision, only as narrative.
///
/// This filter decides what is rendered at all. It is not what decides how the
/// rendered text is READ: everything this module emits, public log lines
/// included, is quoted inside [`crate::prompt::untrusted_block`], under the
/// declaration in [`crate::prompt::UNTRUSTED_DATA_DECLARATION`]. The two are
/// independent and both are required. Excluding a channel keeps text out of
/// the prompt; the fence governs the text that legitimately belongs there. A
/// player's display name, which other people choose, is one channel kept out:
/// a line names each player by seat instead ([`render_log_entry`]).
fn is_prompt_safe(entry: &GameLogEntry) -> bool {
    matches!(entry.presentation.visibility, LogVisibility::Public)
        && !matches!(entry.category, LogCategory::Debug)
}

/// Flatten an engine-authored log entry's segments into one sentence, as
/// `viewer` reads it.
///
/// The engine already decided what this entry says and who may see it; this
/// drops the presentation markup and names each player by seat
/// ([`seat_label`]). The seat is the `player_id` the engine recorded, not the
/// name it wrote for the on-screen log, so the line names a player as
/// `viewer`'s position labels that seat. The rest of the line is the
/// engine's, verbs included, so a line about the viewer's own seat reads
/// `You casts Shock`: agreeing the verb would mean rewriting log text, which
/// this module never does.
pub fn render_log_entry(entry: &GameLogEntry, viewer: PlayerId) -> String {
    let text = entry
        .segments
        .iter()
        .map(|segment| match segment {
            LogSegment::Text(text) => text.clone(),
            LogSegment::CardName { name, .. } => name.clone(),
            LogSegment::PlayerName { player_id, .. } => seat_label(*player_id, viewer),
            LogSegment::Number(value) => value.to_string(),
            LogSegment::Mana(symbols) => symbols.clone(),
            LogSegment::Zone(zone) => format!("{zone:?}"),
            LogSegment::Keyword(keyword) => keyword.clone(),
        })
        .collect::<String>();
    one_line(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::identifiers::ObjectId;
    use engine::types::log::LogPresentation;
    use engine::types::phase::Phase;

    const BUDGET: HistoryBudget = HistoryBudget {
        summarized_cycles: 2,
        current_cycle_entries: 40,
    };

    /// The seat these tests' prompts belong to.
    const VIEWER: PlayerId = PlayerId(1);

    /// A player as the engine's log names one with no display name: by seat,
    /// counted from 1 (`engine::game::log`).
    fn logged_player(seat: PlayerId) -> LogSegment {
        LogSegment::PlayerName {
            name: format!("Player {}", seat.0 + 1),
            player_id: seat,
        }
    }

    fn two_seats(current_turn: u32) -> TurnCycle {
        TurnCycle {
            turns_per_cycle: 2,
            current_turn,
        }
    }

    fn entry(turn: u32, text: &str, importance: LogImportance) -> GameLogEntry {
        GameLogEntry {
            seq: 0,
            game_number: 1,
            turn,
            phase: Phase::PreCombatMain,
            category: LogCategory::Stack,
            segments: vec![LogSegment::Text(text.to_string())],
            presentation: LogPresentation {
                importance,
                ..LogPresentation::default()
            },
        }
    }

    fn turn_start(turn: u32) -> GameLogEntry {
        let mut start = entry(turn, "", LogImportance::Essential);
        start.phase = Phase::Untap;
        start.category = LogCategory::Turn;
        start.segments = vec![
            LogSegment::Text("Turn ".to_string()),
            LogSegment::Number(turn as i32),
            LogSegment::Text(" — ".to_string()),
            logged_player(PlayerId(((turn + 1) % 2) as u8)),
        ];
        start.presentation.boundary = LogBoundary::Turn;
        start
    }

    fn render(history: &[GameLogEntry], cycle: TurnCycle, budget: HistoryBudget) -> String {
        let mut out = String::new();
        push_history(&mut out, history, VIEWER, cycle, budget);
        out
    }

    /// A game of `turns` turns in which every turn logs one essential, one
    /// context and one detail event.
    fn game(turns: u32) -> Vec<GameLogEntry> {
        (1..=turns)
            .flat_map(|turn| {
                [
                    turn_start(turn),
                    entry(turn, &format!("cast on {turn}"), LogImportance::Essential),
                    entry(turn, &format!("target on {turn}"), LogImportance::Context),
                    entry(turn, &format!("untap on {turn}"), LogImportance::Detail),
                ]
            })
            .collect()
    }

    #[test]
    fn the_cycle_in_progress_is_verbatim_and_completed_cycles_are_one_line_per_turn() {
        // Turn 6 of a two-seat game: turns 5-6 are in progress, 1-4 completed.
        let out = render(&game(6), two_seats(6), BUDGET);

        let current = out.find("THIS TURN CYCLE").expect("current section");
        let earlier = out.find("EARLIER TURNS").expect("summary section");
        assert!(earlier < current, "{out}");

        // In progress: essential and context events, each on its own line.
        assert!(out.contains("T5 PreCombatMain: cast on 5\n"), "{out}");
        assert!(out.contains("T6 PreCombatMain: target on 6\n"), "{out}");

        // Completed: one line per turn, essential events only.
        assert!(out.contains("T4 (Turn 4 — You): cast on 4\n"), "{out}");
        assert!(!out.contains("target on 4"), "{out}");
        assert_eq!(out.matches("T3 (").count(), 1, "{out}");
    }

    #[test]
    fn detail_entries_never_reach_the_prompt() {
        let out = render(&game(6), two_seats(6), BUDGET);
        // Reach guard: the same turns' essential and context events are here.
        assert!(out.contains("cast on 6"), "{out}");
        assert!(out.contains("target on 6"), "{out}");
        assert!(!out.contains("untap on"), "{out}");
    }

    /// The point of the exercise: history does not grow with the game. A
    /// 40-turn log renders no more than a 6-turn one under the same budget.
    #[test]
    fn the_rendered_history_is_bounded_however_long_the_game_runs() {
        let short = render(&game(6), two_seats(6), BUDGET);
        let long = render(&game(40), two_seats(40), BUDGET);
        assert!(
            long.len() <= short.len() + 64,
            "short={} long={}\n{long}",
            short.len(),
            long.len()
        );
        // Only the two most recent completed cycles survive.
        assert!(long.contains("T35 ("), "{long}");
        assert!(!long.contains("T34 ("), "{long}");
        assert!(!long.contains("cast on 1;"), "{long}");
    }

    #[test]
    fn cycle_width_follows_the_living_seat_count() {
        // Four seats, turn 6: turns 5-8 are the cycle in progress.
        let four = TurnCycle {
            turns_per_cycle: 4,
            current_turn: 6,
        };
        let out = render(&game(6), four, BUDGET);
        assert!(out.contains("T5 PreCombatMain: target on 5"), "{out}");
        assert!(out.contains("T4 (Turn 4"), "{out}");
    }

    #[test]
    fn a_zero_summary_budget_keeps_only_the_cycle_in_progress() {
        let budget = HistoryBudget {
            summarized_cycles: 0,
            current_cycle_entries: 40,
        };
        let out = render(&game(6), two_seats(6), budget);
        assert!(!out.contains("EARLIER TURNS"), "{out}");
        assert!(out.contains("THIS TURN CYCLE"), "{out}");
    }

    #[test]
    fn a_zero_entry_budget_omits_every_section() {
        assert!(render(&game(6), two_seats(6), HistoryBudget::NONE).is_empty());
    }

    #[test]
    fn the_current_cycle_keeps_only_its_trailing_window() {
        let history: Vec<GameLogEntry> = (0..10)
            .map(|index| entry(1, &format!("event {index}"), LogImportance::Essential))
            .collect();
        let budget = HistoryBudget {
            summarized_cycles: 1,
            current_cycle_entries: 3,
        };
        let out = render(&history, two_seats(1), budget);
        assert!(out.contains("event 7"), "{out}");
        assert!(out.contains("event 9"), "{out}");
        assert!(!out.contains("event 6"), "{out}");
    }

    #[test]
    fn a_turn_with_no_essential_events_still_reads_as_a_turn() {
        let history = vec![
            turn_start(1),
            entry(1, "a target", LogImportance::Context),
            turn_start(3),
        ];
        let out = render(&history, two_seats(3), BUDGET);
        assert!(
            out.contains("T1 (Turn 1 — Player 0): nothing notable"),
            "{out}"
        );
    }

    #[test]
    fn a_long_turn_is_clamped_to_one_line() {
        let mut history = vec![turn_start(1)];
        history.extend((0..80).map(|index| {
            entry(
                1,
                &format!("event number {index}"),
                LogImportance::Essential,
            )
        }));
        history.push(turn_start(3));
        let out = render(&history, two_seats(3), BUDGET);
        let line = out
            .lines()
            .find(|line| line.starts_with("T1 ("))
            .expect("turn 1 summary");
        assert!(line.chars().count() <= TURN_SUMMARY_BUDGET + 1, "{line}");
        assert!(line.ends_with('…'), "{line}");
    }

    fn hidden(turn: u32, text: &str) -> GameLogEntry {
        let mut hidden = entry(turn, text, LogImportance::Essential);
        hidden.presentation.visibility = LogVisibility::HiddenInformation;
        hidden
    }

    /// The engine marks card draws `HiddenInformation` because the entry names
    /// the exact card. A prompt leaves the machine entirely, so it must clear
    /// the same bar the on-screen log does — in the summary as well as the
    /// entry-by-entry section.
    #[test]
    fn hidden_information_reaches_neither_section() {
        let history = vec![
            turn_start(1),
            hidden(1, "Player 0 draws Black Lotus"),
            entry(1, "Player 0 plays a land", LogImportance::Essential),
            turn_start(3),
            hidden(3, "Player 0 draws Ancestral Recall"),
            entry(3, "Player 0 passes", LogImportance::Essential),
        ];
        let out = render(&history, two_seats(3), BUDGET);
        assert!(out.contains("plays a land"), "{out}");
        assert!(out.contains("passes"), "{out}");
        assert!(!out.contains("Black Lotus"), "hidden entry leaked: {out}");
        assert!(!out.contains("Ancestral"), "hidden entry leaked: {out}");
    }

    /// A hidden entry must not consume the window either: filtering before
    /// windowing keeps the visible window the size it claims to be.
    #[test]
    fn hidden_entries_do_not_consume_the_window() {
        let mut history: Vec<GameLogEntry> = Vec::new();
        for index in 0..10 {
            history.push(hidden(1, &format!("secret {index}")));
            history.push(entry(
                1,
                &format!("public {index}"),
                LogImportance::Essential,
            ));
        }
        let budget = HistoryBudget {
            summarized_cycles: 1,
            current_cycle_entries: 3,
        };
        let out = render(&history, two_seats(1), budget);
        for index in 7..10 {
            assert!(out.contains(&format!("public {index}")), "{out}");
        }
        assert!(!out.contains("secret"), "hidden entry leaked: {out}");
        assert!(!out.contains("public 6"), "window overran: {out}");
    }

    #[test]
    fn an_all_hidden_history_renders_no_section() {
        let history = vec![hidden(1, "secret a"), hidden(1, "secret b")];
        assert!(render(&history, two_seats(1), BUDGET).is_empty());
    }

    #[test]
    fn a_log_entry_flattens_to_one_sentence() {
        let mut cast = entry(3, "", LogImportance::Essential);
        cast.segments = vec![
            logged_player(PlayerId(0)),
            LogSegment::Text(" casts ".to_string()),
            LogSegment::CardName {
                name: "Lightning Bolt".to_string(),
                object_id: ObjectId(4),
            },
        ];
        assert_eq!(
            render_log_entry(&cast, VIEWER),
            "Player 0 casts Lightning Bolt"
        );
    }

    /// Issue #10995: a line names each player by seat, in the position's
    /// words, whatever name the engine's log gave them: `You` for the seat
    /// the prompt belongs to, `Player N` counted from 0 for every other.
    #[test]
    fn a_log_entry_names_each_player_by_seat() {
        let mut attack = entry(3, "", LogImportance::Essential);
        attack.segments = vec![
            logged_player(PlayerId(0)),
            LogSegment::Text(" attacks ".to_string()),
            logged_player(VIEWER),
        ];
        assert_eq!(render_log_entry(&attack, VIEWER), "Player 0 attacks You");
        // The same entry in the other seat's prompt. The verb stays the
        // engine's.
        assert_eq!(
            render_log_entry(&attack, PlayerId(0)),
            "You attacks Player 1"
        );

        // A display name the player chose does not reach the line either.
        attack.segments[0] = LogSegment::PlayerName {
            name: "Alice".to_string(),
            player_id: PlayerId(0),
        };
        assert_eq!(render_log_entry(&attack, VIEWER), "Player 0 attacks You");
    }

    // ── Since the seat's last decision ───────────────────────────────────

    /// `game(turns)` with each entry numbered the way the client numbers the
    /// log it accumulates: one `seq` per entry, in order.
    fn numbered_game(turns: u32) -> Vec<GameLogEntry> {
        game(turns)
            .into_iter()
            .zip(0u32..)
            .map(|(mut entry, seq)| {
                entry.seq = seq;
                entry
            })
            .collect()
    }

    /// The opening section of a continue, after the entry numbered `seen`,
    /// with the newest `limit` timeline entries shown entry by entry.
    fn since(history: &[GameLogEntry], seen: Option<u32>, limit: usize) -> String {
        let mut out = String::new();
        let unseen = unseen_timeline_entries(history, seen);
        push_since_last_decision(&mut out, &unseen, VIEWER, limit);
        out
    }

    #[test]
    fn only_entries_after_the_last_answer_are_rendered() {
        // Four entries per turn: entries 0-7 are turns 1-2, 8-11 turn 3.
        let history = numbered_game(3);
        let out = since(&history, Some(7), BUDGET.current_cycle_entries);
        assert!(out.starts_with("--- SINCE YOUR LAST DECISION"), "{out}");
        assert!(out.contains("T3 PreCombatMain: cast on 3"), "{out}");
        assert!(out.contains("T3 PreCombatMain: target on 3"), "{out}");
        for earlier in ["cast on 1", "cast on 2", "target on 2"] {
            assert!(
                !out.contains(earlier),
                "{earlier} was already answered: {out}"
            );
        }
        // The same timeline filter as the cycle in progress.
        assert!(!out.contains("untap on 3"), "{out}");
    }

    #[test]
    fn a_thread_with_nothing_new_says_so() {
        let history = numbered_game(2);
        let newest = history.last().map(|entry| entry.seq);
        let out = since(&history, newest, BUDGET.current_cycle_entries);
        assert!(out.contains("Nothing new has been logged."), "{out}");
    }

    #[test]
    fn unseen_entries_keep_the_prompt_filters_and_are_all_rendered() {
        let history = numbered_game(30);
        let unseen = unseen_timeline_entries(&history, None);
        // Three timeline entries per turn: the turn start, the cast and the
        // target; the detail entry is not a timeline entry.
        assert_eq!(unseen.len(), 90);
        let out = since(&history, None, unseen.len());
        assert!(
            out.contains("T1 PreCombatMain: cast on 1\n")
                && out.contains("T30 PreCombatMain: cast on 30\n"),
            "{out}"
        );

        let mut secret = hidden(31, "Player 0 draws Black Lotus");
        secret.seq = 500;
        assert!(unseen_timeline_entries(&[secret.clone()], None).is_empty());
        assert!(!since(&[secret], None, BUDGET.current_cycle_entries).contains("Black Lotus"));
    }

    /// Unseen entries that exactly fill the window are all shown entry by
    /// entry: one section, every entry on its own line, and no line of key
    /// events.
    #[test]
    fn unseen_entries_that_fit_the_window_are_all_shown_entry_by_entry() {
        // Three turns, three timeline entries each.
        let history = numbered_game(3);
        let out = since(&history, None, 9);
        assert!(out.starts_with("--- SINCE YOUR LAST DECISION (oldest first) ---\n"));
        assert!(!out.contains("EARLIER"), "{out}");
        assert!(out.contains("T1 Untap: Turn 1 — Player 0\n"), "{out}");
        assert!(out.contains("T1 PreCombatMain: target on 1\n"), "{out}");
        assert!(out.contains("T3 PreCombatMain: target on 3\n"), "{out}");
    }

    /// The summary lines and the entry-by-entry lines of a continue's opening
    /// section, each without its heading.
    fn folded_and_shown(out: &str) -> (Vec<&str>, Vec<&str>) {
        let earlier = out
            .find("--- SINCE YOUR LAST DECISION, EARLIER (key events, one line per turn) ---\n")
            .expect("a section for the entries older than the window");
        let latest = out
            .find("--- SINCE YOUR LAST DECISION (oldest first) ---\n")
            .expect("a section for the window");
        assert!(earlier < latest, "{out}");
        let (summaries, window) = out.split_at(latest);
        (
            summaries.lines().skip(1).collect(),
            window.lines().skip(1).collect(),
        )
    }

    /// Entries older than the window move into one line per turn, carrying
    /// the turn's key events, and the window stays entry by entry. A turn the
    /// split falls inside shows on both sides of it, its line marked as
    /// continued, and a context event older than the window is not carried.
    #[test]
    fn unseen_entries_past_the_window_fold_into_one_line_per_turn() {
        let history = numbered_game(3);
        // Nine timeline entries, four shown entry by entry: turn 1 and the
        // start and cast of turn 2 are older than the window.
        let out = since(&history, None, 4);
        let (summaries, window) = folded_and_shown(&out);

        assert_eq!(
            summaries,
            [
                "T1 (Turn 1 — Player 0): cast on 1",
                "T2 (Turn 2 — You, continued below): cast on 2",
            ],
            "{out}"
        );
        // The key events of the turns older than the window, not their
        // context events.
        assert!(!out.contains("target on 1"), "{out}");
        assert_eq!(
            window,
            [
                "T2 PreCombatMain: target on 2",
                "T3 Untap: Turn 3 — Player 0",
                "T3 PreCombatMain: cast on 3",
                "T3 PreCombatMain: target on 3",
            ],
            "{out}"
        );
    }

    /// The split falls right after a turn's start: that turn's older entries
    /// hold no key event, and its line says so without calling the turn
    /// quiet, because its key events follow entry by entry.
    #[test]
    fn a_turn_continued_below_is_not_summarized_as_quiet() {
        let history = numbered_game(3);
        // Nine timeline entries, five shown entry by entry: turn 1 and the
        // start of turn 2 are older than the window.
        let out = since(&history, None, 5);
        let (summaries, window) = folded_and_shown(&out);

        assert_eq!(
            summaries,
            [
                "T1 (Turn 1 — Player 0): cast on 1",
                "T2 (Turn 2 — You, continued below): nothing notable yet",
            ],
            "{out}"
        );
        assert_eq!(
            window.first(),
            Some(&"T2 PreCombatMain: cast on 2"),
            "{out}"
        );
    }
}

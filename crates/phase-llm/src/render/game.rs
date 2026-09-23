//! Text rendering of a game position for an LLM opponent.
//!
//! The input is a VIEWER-FILTERED state
//! (`engine::game::visibility::filter_state_for_viewer`), which is what keeps an
//! LLM seat honest: it is shown its own hand, every public zone, and nothing
//! else. Rendering a filtered state rather than redacting an authoritative one
//! here means the no-cheating property is enforced by the engine's existing
//! visibility authority, not by this module remembering to omit a field.

use std::collections::BTreeMap;

use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::log::{GameLogEntry, LogCategory, LogSegment, LogVisibility};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::text::{clamp_text, mana_cost_text, one_line, type_line_text};

/// How much of the position to render. Driven by difficulty so a low-difficulty
/// seat genuinely reasons from less information (see
/// [`crate::prompt::history_window`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameRenderOptions {
    /// Trailing game-log entries to include. `0` omits the history section.
    pub history_lines: usize,
    /// Include Oracle text for cards in the viewer's hand and on the
    /// battlefield. Off for the lowest difficulties, which are meant to play
    /// off the board rather than off exact card text.
    pub include_oracle_text: bool,
    /// Characters of Oracle text to keep per card.
    pub oracle_text_budget: usize,
}

impl Default for GameRenderOptions {
    fn default() -> Self {
        GameRenderOptions {
            history_lines: 40,
            include_oracle_text: true,
            oracle_text_budget: 320,
        }
    }
}

/// Render the whole position: turn, players, stack, battlefield, the viewer's
/// hand, graveyards, visible exile, and recent history.
pub fn render_board(
    state: &GameState,
    viewer: PlayerId,
    db: Option<&CardDatabase>,
    history: &[GameLogEntry],
    options: &GameRenderOptions,
) -> String {
    let mut out = String::new();
    push_header(&mut out, state, viewer);
    push_players(&mut out, state, viewer);
    push_stack(&mut out, state, viewer);
    push_combat(&mut out, state, viewer);
    push_battlefield(&mut out, state, viewer, db, options);
    push_hand(&mut out, state, viewer, db, options);
    push_graveyards(&mut out, state, viewer);
    push_exile(&mut out, state, viewer);
    push_history(&mut out, history, options.history_lines);
    out
}

/// `You` for the seat the prompt belongs to, `Player N` otherwise. One
/// authority so the same seat never reads two ways in one prompt.
fn seat_label(player: PlayerId, viewer: PlayerId) -> String {
    if player == viewer {
        "You".to_string()
    } else {
        format!("Player {}", player.0)
    }
}

fn push_header(out: &mut String, state: &GameState, viewer: PlayerId) {
    out.push_str(&format!(
        "=== POSITION ===\nYou are Player {}.\nTurn {} — {:?} — active player: {}\nPriority: {}\n",
        viewer.0,
        state.turn_number,
        state.phase,
        seat_label(state.active_player, viewer),
        seat_label(state.priority_player, viewer),
    ));
}

fn push_players(out: &mut String, state: &GameState, viewer: PlayerId) {
    out.push_str("\n--- PLAYERS ---\n");
    for player in &state.players {
        let mut facts = vec![
            format!("{} life", player.life),
            format!("{} cards in hand", player.hand.len()),
            format!("{} cards in library", player.library.len()),
            format!("{} cards in graveyard", player.graveyard.len()),
        ];
        if player.poison_counters > 0 {
            facts.push(format!("{} poison", player.poison_counters));
        }
        if player.energy > 0 {
            facts.push(format!("{} energy", player.energy));
        }
        facts.push(format!(
            "{} lands played this turn",
            player.lands_played_this_turn
        ));
        out.push_str(&format!(
            "{}: {}\n",
            seat_label(player.id, viewer),
            facts.join(", ")
        ));
    }
}

fn push_stack(out: &mut String, state: &GameState, viewer: PlayerId) {
    if state.stack.is_empty() {
        out.push_str("\n--- STACK ---\n(empty)\n");
        return;
    }
    out.push_str("\n--- STACK (top resolves first) ---\n");
    // CR 405.1: the stack is last-in, first-out; render it that way rather than
    // in storage order, so "top" in the prompt means what it means in the rules.
    for (index, entry) in state.stack.iter().rev().enumerate() {
        let name = object_name(state, entry.id)
            .or_else(|| object_name(state, entry.source_id))
            .unwrap_or_else(|| "unknown".to_string());
        out.push_str(&format!(
            "{}. {} (controlled by {})\n",
            index + 1,
            name,
            seat_label(entry.controller, viewer)
        ));
    }
}

fn push_combat(out: &mut String, state: &GameState, viewer: PlayerId) {
    let Some(combat) = state.combat.as_ref() else {
        return;
    };
    if combat.attackers.is_empty() {
        return;
    }
    out.push_str("\n--- COMBAT ---\n");
    for attacker in &combat.attackers {
        let name = object_name(state, attacker.object_id).unwrap_or_else(|| "unknown".to_string());
        let target = match attacker.attack_target {
            AttackTarget::Player(player) => seat_label(player, viewer),
            AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => {
                object_name(state, id).unwrap_or_else(|| "unknown".to_string())
            }
        };
        let blockers = combat
            .blocker_assignments
            .get(&attacker.object_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| object_name(state, *id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let blocked = if blockers.is_empty() {
            if attacker.blocked {
                // CR 509.1h: an attacker stays blocked even with no blockers left.
                " — blocked (no blockers remain)".to_string()
            } else {
                " — unblocked".to_string()
            }
        } else {
            format!(" — blocked by {}", blockers.join(", "))
        };
        out.push_str(&format!("{name} attacking {target}{blocked}\n"));
    }
}

fn push_battlefield(
    out: &mut String,
    state: &GameState,
    viewer: PlayerId,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) {
    out.push_str("\n--- BATTLEFIELD ---\n");
    // Grouped by controller and rendered in seat order so the same board always
    // renders the same way; `state.battlefield` order is preserved within a seat.
    let mut by_controller: BTreeMap<u8, Vec<ObjectId>> = BTreeMap::new();
    for id in state.battlefield.iter() {
        if let Some(object) = state.objects.get(id) {
            by_controller
                .entry(object.controller.0)
                .or_default()
                .push(*id);
        }
    }
    if by_controller.is_empty() {
        out.push_str("(empty)\n");
        return;
    }
    for (controller, ids) in by_controller {
        out.push_str(&format!("{}:\n", seat_label(PlayerId(controller), viewer)));
        for id in ids {
            out.push_str(&format!("  - {}\n", permanent_line(state, id, db, options)));
        }
    }
}

fn push_hand(
    out: &mut String,
    state: &GameState,
    viewer: PlayerId,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) {
    let Some(player) = state.players.iter().find(|player| player.id == viewer) else {
        return;
    };
    out.push_str("\n--- YOUR HAND ---\n");
    if player.hand.is_empty() {
        out.push_str("(empty)\n");
        return;
    }
    for id in player.hand.iter() {
        out.push_str(&format!("  - {}\n", card_line(state, *id, db, options)));
    }
}

fn push_graveyards(out: &mut String, state: &GameState, viewer: PlayerId) {
    out.push_str("\n--- GRAVEYARDS ---\n");
    for player in &state.players {
        let names: Vec<String> = player
            .graveyard
            .iter()
            .filter_map(|id| object_name(state, *id))
            .collect();
        out.push_str(&format!(
            "{}: {}\n",
            seat_label(player.id, viewer),
            if names.is_empty() {
                "(empty)".to_string()
            } else {
                names.join(", ")
            }
        ));
    }
}

fn push_exile(out: &mut String, state: &GameState, viewer: PlayerId) {
    // CR 406.1: exile is a public zone, but face-down exiled cards are not
    // public. The filtered state has already replaced any name this seat may not
    // read, so rendering names here cannot leak.
    let names: Vec<String> = state
        .exile
        .iter()
        .filter_map(|id| {
            let object = state.objects.get(id)?;
            Some(format!(
                "{} (owned by {})",
                one_line(&object.name),
                seat_label(object.owner, viewer)
            ))
        })
        .collect();
    if names.is_empty() {
        return;
    }
    out.push_str("\n--- EXILE ---\n");
    out.push_str(&names.join(", "));
    out.push('\n');
}

fn push_history(out: &mut String, history: &[GameLogEntry], limit: usize) {
    if limit == 0 || history.is_empty() {
        return;
    }
    // Filter BEFORE windowing so dropped entries do not consume the budget —
    // otherwise a burst of draws would silently shorten the visible history.
    let visible: Vec<&GameLogEntry> = history
        .iter()
        .filter(|entry| is_prompt_safe(entry))
        .collect();
    if visible.is_empty() {
        return;
    }
    out.push_str("\n--- RECENT GAME HISTORY (oldest first) ---\n");
    let start = visible.len().saturating_sub(limit);
    for entry in &visible[start..] {
        out.push_str(&format!(
            "T{} {:?}: {}\n",
            entry.turn,
            entry.phase,
            render_log_entry(entry)
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
/// rendered text is READ: everything this module emits — including public log
/// lines, whose `LogSegment::PlayerName` text is chosen by other people — is
/// quoted inside [`crate::prompt::untrusted_block`], under the declaration in
/// [`crate::prompt::UNTRUSTED_DATA_DECLARATION`]. The two are independent and
/// both are required. Excluding a channel keeps text out of the prompt; the
/// fence governs the text that legitimately belongs there.
fn is_prompt_safe(entry: &GameLogEntry) -> bool {
    matches!(entry.presentation.visibility, LogVisibility::Public)
        && !matches!(entry.category, LogCategory::Debug)
}

/// Flatten an engine-authored log entry's segments into one sentence. The
/// engine already decided what this entry says and who may see it; this only
/// drops the presentation markup.
pub fn render_log_entry(entry: &GameLogEntry) -> String {
    let text = entry
        .segments
        .iter()
        .map(|segment| match segment {
            LogSegment::Text(text) => text.clone(),
            LogSegment::CardName { name, .. } => name.clone(),
            LogSegment::PlayerName { name, .. } => name.clone(),
            LogSegment::Number(value) => value.to_string(),
            LogSegment::Mana(symbols) => symbols.clone(),
            LogSegment::Zone(zone) => format!("{zone:?}"),
            LogSegment::Keyword(keyword) => keyword.clone(),
        })
        .collect::<String>();
    one_line(&text)
}

/// A battlefield permanent: identity plus the state that changes how it plays.
fn permanent_line(
    state: &GameState,
    id: ObjectId,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) -> String {
    let Some(object) = state.objects.get(&id) else {
        return format!("object #{}", id.0);
    };
    let mut parts = vec![one_line(&object.name)];

    let type_line = type_line_text(&object.card_types);
    if !type_line.is_empty() {
        parts.push(type_line);
    }
    if let (Some(power), Some(toughness)) = (object.power, object.toughness) {
        parts.push(format!("{power}/{toughness}"));
    }
    if let Some(loyalty) = object.loyalty {
        parts.push(format!("loyalty {loyalty}"));
    }
    parts.push(if object.tapped { "tapped" } else { "untapped" }.to_string());
    if object.summoning_sick {
        // CR 302.6: only matters for attacking and {T} abilities, but it is the
        // single most common reason a plausible play is illegal.
        parts.push("summoning sick".to_string());
    }
    if object.face_down {
        parts.push("face down".to_string());
    }
    if object.damage_marked > 0 {
        parts.push(format!("{} damage marked", object.damage_marked));
    }
    let counters = counter_text(object);
    if !counters.is_empty() {
        parts.push(counters);
    }
    if !object.attachments.is_empty() {
        let attached: Vec<String> = object
            .attachments
            .iter()
            .filter_map(|attached| object_name(state, *attached))
            .collect();
        if !attached.is_empty() {
            parts.push(format!("attached: {}", attached.join(", ")));
        }
    }
    if !object.keywords.is_empty() {
        parts.push(
            object
                .keywords
                .iter()
                .map(|keyword| format!("{keyword:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    let mut line = parts.join(" | ");
    if let Some(text) = oracle_text(object.name.as_str(), db, options) {
        line.push_str(&format!(" | \"{text}\""));
    }
    line
}

/// A card in hand: what it is and what it costs, which is the pair that decides
/// whether it is castable this turn.
fn card_line(
    state: &GameState,
    id: ObjectId,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) -> String {
    let Some(object) = state.objects.get(&id) else {
        return format!("object #{}", id.0);
    };
    let mut parts = vec![one_line(&object.name)];
    let cost = mana_cost_text(&object.mana_cost);
    if !cost.is_empty() {
        parts.push(cost);
    }
    let type_line = type_line_text(&object.card_types);
    if !type_line.is_empty() {
        parts.push(type_line);
    }
    if let (Some(power), Some(toughness)) = (object.power, object.toughness) {
        parts.push(format!("{power}/{toughness}"));
    }
    let mut line = parts.join(" | ");
    if let Some(text) = oracle_text(object.name.as_str(), db, options) {
        line.push_str(&format!(" | \"{text}\""));
    }
    line
}

fn counter_text(object: &engine::game::game_object::GameObject) -> String {
    if object.counters.is_empty() {
        return String::new();
    }
    // `BTreeMap` for a deterministic order: `counters` is a `HashMap`, and an
    // unordered render would make the decision fingerprint unstable.
    let ordered: BTreeMap<String, u32> = object
        .counters
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| (format!("{kind:?}"), *count))
        .collect();
    if ordered.is_empty() {
        return String::new();
    }
    ordered
        .into_iter()
        .map(|(kind, count)| format!("{count} {kind} counter(s)"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn oracle_text(
    name: &str,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) -> Option<String> {
    if !options.include_oracle_text {
        return None;
    }
    let text = db?.get_face_by_name(name)?.oracle_text.as_deref()?;
    let collapsed = one_line(text);
    (!collapsed.is_empty()).then(|| clamp_text(&collapsed, options.oracle_text_budget))
}

/// Every object name this module prints comes through here or through a direct
/// `one_line(&object.name)`. A name is rendered data and may carry line breaks;
/// folding it keeps a name from opening a line of its own that reads as a
/// section heading or a counterfeit `[n]` option.
fn object_name(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects.get(&id).map(|object| one_line(&object.name))
}

/// Objects a seat can see in a zone. Exposed for callers that want to describe
/// a zone without rendering the whole board.
pub fn zone_names(state: &GameState, zone: Zone) -> Vec<String> {
    state
        .objects
        .iter()
        .filter(|(_, object)| object.zone == zone)
        .map(|(_, object)| one_line(&object.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::log::{GameLogEntry, LogCategory, LogPresentation};
    use engine::types::phase::Phase;

    fn log_entry(segments: Vec<LogSegment>) -> GameLogEntry {
        GameLogEntry {
            seq: 0,
            turn: 3,
            phase: Phase::PreCombatMain,
            category: LogCategory::Stack,
            segments,
            presentation: LogPresentation::default(),
        }
    }

    #[test]
    fn a_log_entry_flattens_to_one_sentence() {
        let entry = log_entry(vec![
            LogSegment::PlayerName {
                name: "Player 1".to_string(),
                player_id: PlayerId(1),
            },
            LogSegment::Text(" casts ".to_string()),
            LogSegment::CardName {
                name: "Lightning Bolt".to_string(),
                object_id: ObjectId(4),
            },
        ]);
        assert_eq!(render_log_entry(&entry), "Player 1 casts Lightning Bolt");
    }

    #[test]
    fn the_history_section_keeps_only_the_trailing_window() {
        let history: Vec<GameLogEntry> = (0..10)
            .map(|index| log_entry(vec![LogSegment::Text(format!("event {index}"))]))
            .collect();
        let mut out = String::new();
        push_history(&mut out, &history, 3);
        assert!(out.contains("event 7"));
        assert!(out.contains("event 9"));
        assert!(!out.contains("event 6"));
    }

    fn hidden_entry(text: &str) -> GameLogEntry {
        let mut entry = log_entry(vec![LogSegment::Text(text.to_string())]);
        entry.presentation.visibility = LogVisibility::HiddenInformation;
        entry
    }

    /// The engine marks card draws `HiddenInformation` because the entry names
    /// the exact card. A prompt leaves the machine entirely, so it must clear
    /// the same bar the on-screen log does.
    #[test]
    fn hidden_information_entries_never_reach_the_prompt() {
        let history = vec![
            log_entry(vec![LogSegment::Text("Player 1 plays a land".to_string())]),
            hidden_entry("Player 0 draws Black Lotus"),
            log_entry(vec![LogSegment::Text("Player 1 passes".to_string())]),
        ];

        let mut out = String::new();
        push_history(&mut out, &history, 40);

        assert!(out.contains("plays a land"), "{out}");
        assert!(out.contains("passes"), "{out}");
        assert!(!out.contains("Black Lotus"), "hidden entry leaked: {out}");
        assert!(!out.contains("draws"), "hidden entry leaked: {out}");
    }

    /// A hidden entry must not consume the history budget either: filtering
    /// before windowing keeps the visible window the size it claims to be.
    #[test]
    fn hidden_entries_do_not_consume_the_history_window() {
        let mut history: Vec<GameLogEntry> = Vec::new();
        for index in 0..10 {
            history.push(hidden_entry(&format!("secret {index}")));
            history.push(log_entry(vec![LogSegment::Text(format!("public {index}"))]));
        }

        let mut out = String::new();
        push_history(&mut out, &history, 3);

        for index in 7..10 {
            assert!(out.contains(&format!("public {index}")), "{out}");
        }
        assert!(!out.contains("secret"), "hidden entry leaked: {out}");
        assert!(!out.contains("public 6"), "window overran: {out}");
    }

    /// A history made up entirely of hidden entries yields no section at all,
    /// rather than an empty heading that implies nothing happened.
    #[test]
    fn an_all_hidden_history_renders_no_section() {
        let history = vec![hidden_entry("secret a"), hidden_entry("secret b")];

        let mut out = String::new();
        push_history(&mut out, &history, 40);

        assert!(out.is_empty(), "{out}");
    }

    #[test]
    fn a_zero_history_window_omits_the_section_entirely() {
        let history = vec![log_entry(vec![LogSegment::Text("event".to_string())])];
        let mut out = String::new();
        push_history(&mut out, &history, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn the_viewers_seat_reads_as_you_and_others_by_number() {
        assert_eq!(seat_label(PlayerId(1), PlayerId(1)), "You");
        assert_eq!(seat_label(PlayerId(0), PlayerId(1)), "Player 0");
    }
}

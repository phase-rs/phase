//! Text rendering of a game position for an LLM opponent.
//!
//! The input is a VIEWER-FILTERED state
//! (`engine::game::visibility::filter_state_for_viewer`), which is what keeps an
//! LLM seat honest: it is shown its own hand, every public zone, and nothing
//! else. Rendering a filtered state rather than redacting an authoritative one
//! here means the no-cheating property is enforced by the engine's existing
//! visibility authority, not by this module remembering to omit a field.

use std::collections::{BTreeMap, BTreeSet};

use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::mana_sources;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaType;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::action::describe_tagged;
use super::history::{push_history, HistoryBudget, TurnCycle};
use super::text::{clamp_text, mana_cost_text, one_line, type_line_text};

/// How much of the position to render. Driven by difficulty so a low-difficulty
/// seat genuinely reasons from less information (see
/// [`crate::prompt::history_budget`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameRenderOptions {
    /// How much game history to include, summarized per turn cycle (see
    /// [`super::history`]).
    pub history: HistoryBudget,
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
            history: HistoryBudget {
                summarized_cycles: 2,
                current_cycle_entries: 40,
            },
            include_oracle_text: true,
            oracle_text_budget: 320,
        }
    }
}

/// Render the whole position: turn, players, stack, pending delayed triggers,
/// battlefield, the viewer's hand and available mana, what the viewer knows of
/// other hands, graveyards, visible exile, and the turn-cycle history.
///
/// Every section is a function of live state except the history, so the size
/// of a prompt tracks the size of the position, not the length of the game.
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
    push_delayed_triggers(&mut out, state, viewer);
    push_combat(&mut out, state, viewer);
    push_battlefield(&mut out, state, viewer, db, options);
    push_hand(&mut out, state, viewer, db, options);
    push_available_mana(&mut out, state, viewer);
    push_known_other_hands(&mut out, state, viewer);
    push_graveyards(&mut out, state, viewer);
    push_exile(&mut out, state, viewer);
    push_history(&mut out, history, TurnCycle::of(state), options.history);
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
            format!("{} cards in library", state.library_of(player.id).len()),
            format!("{} cards in graveyard", state.graveyard_of(player.id).len()),
        ];
        if player.poison_counters > 0 {
            facts.push(format!("{} poison", player.poison_counters));
        }
        if player.energy > 0 {
            facts.push(format!("{} energy", player.energy));
        }
        // CR 106.4: unspent mana stays in a player's pool until the step or
        // phase ends, so it is mana that player can still spend right now.
        if !player.mana_pool.is_empty() {
            facts.push(format!(
                "{} unspent mana in pool ({})",
                player.mana_pool.total(),
                mana_symbols(player.mana_pool.units().map(|unit| unit.color))
            ));
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

/// Effects already set to happen later: CR 603.7a delayed triggered abilities
/// created by resolved spells and abilities ("at the beginning of the next end
/// step, sacrifice it"). They are not on the stack yet and not on any card on
/// the battlefield, so without this section a seat cannot see them coming.
fn push_delayed_triggers(out: &mut String, state: &GameState, viewer: PlayerId) {
    if state.delayed_triggers.is_empty() {
        return;
    }
    out.push_str("\n--- PENDING DELAYED TRIGGERS ---\n");
    for trigger in &state.delayed_triggers {
        let source = object_name(state, trigger.source_id).unwrap_or_else(|| "unknown".to_string());
        let when = serde_json::to_value(&trigger.condition)
            .map(|value| describe_tagged(state, &value))
            .unwrap_or_else(|_| "later".to_string());
        out.push_str(&format!(
            "- from {source} (controlled by {}): {when}\n",
            seat_label(trigger.controller, viewer)
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
        let lines = ids
            .into_iter()
            .map(|id| permanent_line(state, id, db, options));
        for line in grouped_lines(lines) {
            out.push_str(&format!("  - {line}\n"));
        }
    }
}

/// Collapse identical lines into one `line ×N`, in first-seen order.
///
/// Lossless: two permanents render to the same line only when every fact the
/// prompt states about them — name, type, P/T, tapped, counters, damage,
/// attachments, keywords, rules text — is the same. Five untapped Forests are
/// one line, not five copies of the same text.
fn grouped_lines(lines: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        let count = counts.entry(line.clone()).or_insert(0);
        if *count == 0 {
            order.push(line);
        }
        *count += 1;
    }
    order
        .into_iter()
        .map(|line| match counts.get(&line) {
            Some(count) if *count > 1 => format!("{line} ×{count}"),
            _ => line,
        })
        .collect()
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
    let lines = player
        .hand
        .iter()
        .map(|id| card_line(state, *id, db, options));
    for line in grouped_lines(lines) {
        out.push_str(&format!("  - {line}\n"));
    }
}

/// The viewer's mana: what is floating now and every untapped source that can
/// still produce more — lands, but equally mana artifacts, mana creatures and
/// Treasures.
///
/// CR 605.1a: a mana ability is any non-loyalty activated ability that could
/// add mana without targeting. The engine's own enumeration
/// (`mana_sources::activatable_mana_options`) decides which permanents qualify
/// and whether each can be activated right now (untapped, not summoning sick
/// for a `{T}` cost), so a Sol Ring is listed beside the lands exactly when the
/// engine would tap it to pay for a spell.
fn push_available_mana(out: &mut String, state: &GameState, viewer: PlayerId) {
    let Some(player) = state.players.iter().find(|player| player.id == viewer) else {
        return;
    };
    let pool: Vec<ManaType> = player.mana_pool.units().map(|unit| unit.color).collect();

    let mut total = 0u32;
    let mut sources: Vec<String> = Vec::new();
    for id in state.battlefield.iter() {
        let options = mana_sources::activatable_mana_options(state, *id, viewer);
        if options.is_empty() {
            continue;
        }
        let Some(object) = state.objects.get(id) else {
            continue;
        };
        let produced: BTreeSet<ManaType> = options.iter().map(|option| option.mana_type).collect();
        // The engine's net figure: a source whose activation costs as much
        // mana as it makes adds nothing, and is not counted as if it did.
        let amount = mana_sources::max_mana_yield(state, *id, viewer);
        total = total.saturating_add(amount);
        // Core types only: "Artifact" is the fact that matters here, and the
        // full type line is already on the battlefield entry above.
        let kind = object
            .card_types
            .core_types
            .iter()
            .map(|core| format!("{core:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        sources.push(format!(
            "{} ({kind}): {amount} mana, {}",
            one_line(&object.name),
            mana_symbols(produced.into_iter())
        ));
    }

    if pool.is_empty() && sources.is_empty() {
        return;
    }
    out.push_str("\n--- YOUR AVAILABLE MANA ---\n");
    if !pool.is_empty() {
        out.push_str(&format!(
            "Unspent mana in your pool: {}\n",
            mana_symbols(pool.into_iter())
        ));
    }
    if sources.is_empty() {
        out.push_str("No untapped mana sources.\n");
        return;
    }
    out.push_str(&format!(
        "Untapped mana sources (up to {total} more mana available now):\n"
    ));
    for line in grouped_lines(sources) {
        out.push_str(&format!("  - {line}\n"));
    }
}

/// `{W}{U}{C}` for a run of mana types, in the order given.
fn mana_symbols(types: impl Iterator<Item = ManaType>) -> String {
    types
        .map(|mana| match mana {
            ManaType::White => "{W}",
            ManaType::Blue => "{U}",
            ManaType::Black => "{B}",
            ManaType::Red => "{R}",
            ManaType::Green => "{G}",
            ManaType::Colorless => "{C}",
        })
        .collect()
}

/// Cards in another player's hand that this seat has legitimately seen —
/// revealed by an effect, or otherwise known.
///
/// CR 400.2 + CR 402.3: a hand is a hidden zone; another player's cards are
/// normally visible only as a count. The viewer-filtered state the board is
/// rendered from has already redacted every card this seat may not identify
/// (`hide_card` turns it face down and strips its name), so any card in another
/// player's hand still face up here is one the engine says this seat knows — a
/// revealed card, or a teammate's hand where the format shares it.
fn push_known_other_hands(out: &mut String, state: &GameState, viewer: PlayerId) {
    let mut lines: Vec<String> = Vec::new();
    for player in state.players.iter().filter(|player| player.id != viewer) {
        let known: Vec<String> = player
            .hand
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| !object.face_down)
            .map(|object| one_line(&object.name))
            .collect();
        if !known.is_empty() {
            lines.push(format!(
                "{}: {} (of {} in hand)",
                seat_label(player.id, viewer),
                known.join(", "),
                player.hand.len()
            ));
        }
    }
    if lines.is_empty() {
        return;
    }
    out.push_str("\n--- CARDS YOU KNOW IN OTHER HANDS ---\n");
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
}

fn push_graveyards(out: &mut String, state: &GameState, viewer: PlayerId) {
    out.push_str("\n--- GRAVEYARDS ---\n");
    for player in &state.players {
        let names = grouped_lines(
            state
                .graveyard_of(player.id)
                .iter()
                .filter_map(|id| object_name(state, *id)),
        );
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
    use engine::game::create_object;
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::format::FormatConfig;
    use engine::types::identifiers::CardId;

    fn rendered(state: &GameState, viewer: PlayerId) -> String {
        let visible = filter_state_for_viewer(state, viewer);
        render_board(&visible, viewer, None, &[], &GameRenderOptions::default())
    }

    #[test]
    fn identical_lines_collapse_with_a_count_in_first_seen_order() {
        let lines = ["Forest", "Bear", "Forest", "Forest", "Elf"].map(str::to_string);
        assert_eq!(grouped_lines(lines), vec!["Forest ×3", "Bear", "Elf"]);
    }

    #[test]
    fn identical_permanents_render_once_with_a_count() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        for index in 0..4 {
            create_object(
                &mut state,
                CardId(index),
                PlayerId(1),
                "Mountain".to_string(),
                Zone::Battlefield,
            );
        }
        let board = rendered(&state, PlayerId(1));
        assert_eq!(board.matches("Mountain").count(), 1, "{board}");
        assert!(board.contains("Mountain | untapped ×4"), "{board}");
    }

    #[test]
    fn floating_mana_is_public_and_listed_for_its_owner() {
        use engine::types::mana::ManaUnit;
        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            Vec::new(),
        ));
        let board = rendered(&state, PlayerId(1));
        assert!(
            board.contains("Player 0: ") && board.contains("1 unspent mana in pool ({R})"),
            "{board}"
        );
        let own = rendered(&state, PlayerId(0));
        assert!(own.contains("Unspent mana in your pool: {R}"), "{own}");
    }

    #[test]
    fn floating_mana_lists_every_unit_in_the_order_it_was_added() {
        use engine::types::mana::ManaUnit;
        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        for color in [ManaType::Red, ManaType::Green, ManaType::Red] {
            let unit = ManaUnit::new(color, ObjectId(0), false, Vec::new());
            state.players[0].mana_pool.add(unit);
        }
        let own = rendered(&state, PlayerId(0));
        assert!(own.contains("3 unspent mana in pool ({R}{G}{R})"), "{own}");
        assert!(
            own.contains("Unspent mana in your pool: {R}{G}{R}"),
            "{own}"
        );
    }

    /// A seat that has seen a card in another hand (a reveal) is told so; a
    /// card it has not seen stays a count.
    #[test]
    fn only_known_cards_in_another_hand_are_named() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let seen = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Brainstorm".to_string(),
            Zone::Hand,
        );
        let unseen = rendered(&state, PlayerId(1));
        assert!(!unseen.contains("Counterspell"), "{unseen}");
        assert!(!unseen.contains("CARDS YOU KNOW"), "{unseen}");

        state.revealed_cards.insert(seen);
        let board = rendered(&state, PlayerId(1));
        assert!(
            board.contains("Player 0: Counterspell (of 2 in hand)"),
            "{board}"
        );
        assert!(!board.contains("Brainstorm"), "hidden card leaked: {board}");
    }

    /// A mana artifact is a mana source exactly like a land: the engine taps
    /// it to pay for spells, so the seat is shown it as available mana.
    #[test]
    fn mana_artifacts_count_as_available_mana_beside_lands() {
        use crate::test_support::mana_rock_ability;
        use engine::game::scenario::GameScenario;
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(PlayerId(0), ManaColor::Green);
        scenario.add_basic_land(PlayerId(0), ManaColor::Green);
        scenario
            .add_creature(PlayerId(0), "Sol Ring", 0, 0)
            .as_artifact()
            .with_ability_definition(mana_rock_ability(2));
        let runner = scenario.build();

        let board = rendered(runner.state(), PlayerId(0));
        assert!(board.contains("--- YOUR AVAILABLE MANA ---"), "{board}");
        assert!(
            board.contains("Sol Ring (Artifact): 2 mana, {C}"),
            "{board}"
        );
        assert!(board.contains("Forest (Land): 1 mana, {G} ×2"), "{board}");
        assert!(board.contains("up to 4 more mana available now"), "{board}");

        // Another seat is never shown this seat's sources as its own.
        let other = rendered(runner.state(), PlayerId(1));
        assert!(!other.contains("YOUR AVAILABLE MANA"), "{other}");
    }

    /// CR 603.7a: a delayed trigger waiting to fire is a pending effect the
    /// seat must plan around, though nothing on the stack or board shows it.
    #[test]
    fn pending_delayed_triggers_are_rendered_with_their_source_and_timing() {
        use engine::types::ability::{
            DelayedTriggerCondition, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
        };
        use engine::types::game_state::DelayedTrigger;
        use engine::types::phase::Phase;

        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sneak Attack".to_string(),
            Zone::Battlefield,
        );
        state.delayed_triggers.push(DelayedTrigger::new(
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                source,
                PlayerId(0),
            )),
            PlayerId(0),
            source,
            true,
        ));

        let board = rendered(&state, PlayerId(1));
        assert!(
            board.contains("--- PENDING DELAYED TRIGGERS ---"),
            "{board}"
        );
        assert!(
            board.contains(
                "- from Sneak Attack (controlled by Player 0): At Next Phase (Phase: End)"
            ),
            "{board}"
        );
        assert!(
            !rendered(&GameState::new(FormatConfig::standard(), 2, 3), PlayerId(1))
                .contains("PENDING DELAYED TRIGGERS")
        );
    }

    #[test]
    fn the_viewers_seat_reads_as_you_and_others_by_number() {
        assert_eq!(seat_label(PlayerId(1), PlayerId(1)), "You");
        assert_eq!(seat_label(PlayerId(0), PlayerId(1)), "Player 0");
    }

    #[test]
    fn the_shared_pile_renders_for_every_seat_through_the_viewer_filter() {
        use engine::game::create_object;
        use engine::game::visibility::filter_state_for_viewer;
        use engine::types::format::FormatConfig;
        use engine::types::identifiers::CardId;

        for (format, viewer, other, shared) in [
            (FormatConfig::dandan(), PlayerId(1), PlayerId(0), true),
            (FormatConfig::dandan(), PlayerId(0), PlayerId(1), true),
            (FormatConfig::standard(), PlayerId(1), PlayerId(0), false),
        ] {
            let mut state = GameState::new(format, 2, 3);
            // Standard: the staged cards belong to the viewer's own zones.
            let holder = if shared { PlayerId(0) } else { viewer };
            for (id, name) in ["Island", "Brainstorm", "Mental Note"]
                .into_iter()
                .enumerate()
            {
                create_object(
                    &mut state,
                    CardId(id as u64),
                    holder,
                    name.to_string(),
                    Zone::Library,
                );
            }
            create_object(
                &mut state,
                CardId(9),
                holder,
                "Memory Lapse".to_string(),
                Zone::Graveyard,
            );
            let visible = filter_state_for_viewer(&state, viewer);
            let board = render_board(&visible, viewer, None, &[], &GameRenderOptions::default());

            let line = |label: &str| {
                board
                    .lines()
                    .find(|l| l.starts_with(&format!("{label}: ")) && l.contains("life"))
                    .unwrap_or_else(|| panic!("no players line for {label} in\n{board}"))
                    .to_string()
            };
            assert!(
                line("You").contains("3 cards in library"),
                "{viewer:?} shared={shared}: {board}"
            );
            let other_line = line(&format!("Player {}", other.0));
            assert_eq!(
                other_line.contains("3 cards in library"),
                shared,
                "{viewer:?}: the other seat reads the same pile only when shared"
            );
            assert!(
                board.contains("You: Memory Lapse"),
                "{viewer:?} shared={shared}: the viewer's graveyard line"
            );
        }
    }
}

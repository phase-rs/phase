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
use engine::game::combat::{AttackTarget, AttackerInfo, CombatParticipation};
use engine::game::game_object::GameObject;
use engine::game::{casting, commander, mana_sources, players};
use engine::types::game_state::{GameState, StackEntry};
use engine::types::identifiers::ObjectId;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaType;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::action::describe_tagged;
use super::history::{push_history, HistoryBudget, TurnCycle};
use super::keyword::keyword_text;
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

/// Render the whole position: turn, players, commanders, stack, pending delayed
/// triggers, battlefield, the viewer's hand and available mana, what the viewer
/// knows of other hands, graveyards, visible exile, and the turn-cycle history.
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
    push_commanders(&mut out, state, viewer, db, options);
    push_stack(&mut out, state, viewer);
    push_delayed_triggers(&mut out, state, viewer);
    push_combat(&mut out, state, viewer);
    push_battlefield(&mut out, state, viewer, db, options);
    push_hand(&mut out, state, viewer, db, options);
    push_available_mana(&mut out, state, viewer);
    push_known_other_hands(&mut out, state, viewer);
    push_graveyards(&mut out, state, viewer);
    push_exile(&mut out, state, viewer);
    push_history(
        &mut out,
        history,
        viewer,
        TurnCycle::of(state),
        options.history,
    );
    out
}

/// `You` for the seat the prompt belongs to, `Player N` otherwise. One
/// authority so the same seat never reads two ways in one prompt. The option
/// labels and the history name seats through it too.
pub(crate) fn seat_label(player: PlayerId, viewer: PlayerId) -> String {
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

/// Each seat's commanders: where each one is, what casting it from the command
/// zone costs next, and the commander damage it has dealt.
///
/// CR 400.2 + CR 903.3: the command zone is public, and a card keeps its
/// commander designation in every zone, so a commander is listed wherever it
/// is. CR 800.4a: a player who has left the game takes their cards with them,
/// so only the seats still in the game are listed, and commander damage only
/// to those seats.
///
/// A commander that is face down in the viewer-filtered state (a hidden zone,
/// CR 400.2 + CR 401.2, or a face-down permanent, CR 708.2) is never located.
/// The filter redacts its identity but keeps the commander designation and its
/// object, so saying which of those places holds it would tell this seat which
/// hidden card is the commander, or that a tutor took it from the library.
///
/// Every figure is read from the engine's authority for it, not rebuilt here:
/// - the seat's color identity: `commander::defined_commander_color_identity`
///   (CR 903.4), or the registered identity alone,
///   `commander::registered_commander_color_identity`, while one of the seat's
///   commanders is face down;
/// - the commander tax: `commander::commander_tax` (CR 903.8), the live count
///   of casts from the command zone. The object's derived `commander_tax`
///   field is filled only by `derive_display_state` and is never deserialized;
/// - the whole cost of the next cast: `casting::display_spell_cost`, which adds
///   the tax and applies every cost modifier the cast would;
/// - the damage: `GameState::commander_damage`, and how much more eliminates
///   that player, `commander::commander_lethal_headroom` (CR 903.10a).
fn push_commanders(
    out: &mut String,
    state: &GameState,
    viewer: PlayerId,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) {
    // CR 408.3: only a format that starts cards in the command zone has
    // commanders.
    if !state.format_config.command_zone {
        return;
    }
    // Grouped by owner in seat order and by id within a seat, so the same
    // position always renders the same way: `objects` is a hash map.
    let mut by_owner: BTreeMap<u8, Vec<&GameObject>> = BTreeMap::new();
    for object in state.objects.values() {
        if object.is_commander && players::is_alive(state, object.owner) {
            by_owner.entry(object.owner.0).or_default().push(object);
        }
    }
    if by_owner.is_empty() {
        return;
    }
    out.push_str("\n--- COMMANDERS ---\n");
    for (owner, mut commanders) in by_owner {
        let owner = PlayerId(owner);
        // CR 903.4 + CR 903.5c: the commanders' combined color identity, which
        // every card in that seat's deck falls within. When no deck pool
        // registers the seat's commanders, the engine reads it from the
        // commander objects' printed colors and mana cost, which the filter
        // redacts from a face-down one. While one is face down, only the
        // registered identity is stated, and none when there is none.
        let identity = if commanders.iter().any(|object| object.face_down) {
            commander::registered_commander_color_identity(state, owner)
        } else {
            commander::defined_commander_color_identity(state, owner)
        };
        let identity = match identity {
            Some(colors) if colors.is_empty() => " (color identity: colorless)".to_string(),
            Some(colors) => format!(
                " (color identity: {})",
                mana_symbols(colors.into_iter().map(ManaType::from))
            ),
            None => String::new(),
        };
        out.push_str(&format!("{}{identity}:\n", seat_label(owner, viewer)));
        commanders.sort_by_key(|object| object.id);
        for object in commanders {
            out.push_str(&format!(
                "  - {}\n",
                commander_line(state, viewer, object, db, options)
            ));
            for line in commander_damage_lines(state, viewer, object.id) {
                out.push_str(&format!("    - {line}\n"));
            }
        }
    }
}

/// A commander's line: what it is, where it is, and what its next cast from the
/// command zone costs.
///
/// A commander on the battlefield is named only, since its battlefield line
/// states its characteristics. So is a face-down one, named as the filtered
/// state names it.
fn commander_line(
    state: &GameState,
    viewer: PlayerId,
    object: &GameObject,
    db: Option<&CardDatabase>,
    options: &GameRenderOptions,
) -> String {
    let named_only = object.zone == Zone::Battlefield || object.face_down;
    let mut parts = if named_only {
        vec![one_line(&object.name)]
    } else {
        card_facts(object)
    };
    parts.push(commander_place(viewer, object));
    parts.push(next_cast_text(state, object));
    let mut line = parts.join(" | ");
    if named_only {
        return line;
    }
    if let Some(text) = oracle_text(object.name.as_str(), db, options) {
        line.push_str(&format!(" | \"{text}\""));
    }
    line
}

/// Where a commander is, in the words the rest of the position uses.
///
/// A face-down one is not located (see [`push_commanders`]): the words are
/// the same in a hand, a library, or face down on the battlefield, so they
/// stay the same as it moves between them. CR 108.4 + CR 110.2: a permanent's
/// controller may not be its owner, so the battlefield says who controls it.
fn commander_place(viewer: PlayerId, object: &GameObject) -> String {
    if object.face_down {
        return "face down or in a hidden zone".to_string();
    }
    match object.zone {
        Zone::Command => "in the command zone".to_string(),
        Zone::Battlefield => format!(
            "on the battlefield, controlled by {}",
            seat_label(object.controller, viewer)
        ),
        Zone::Stack => "on the stack".to_string(),
        Zone::Graveyard => "in the graveyard".to_string(),
        Zone::Exile => "in exile".to_string(),
        Zone::Hand => "in hand".to_string(),
        Zone::Library => "in the library".to_string(),
    }
}

/// What casting a commander from the command zone costs next (CR 903.8).
///
/// In the command zone, the whole cost, priced by the engine's cost-display
/// authority for the commander's owner, the one player who may cast it from
/// there. Anywhere else, or when that authority prices no mana cost, the tax
/// alone, which a cast from the command zone adds.
fn next_cast_text(state: &GameState, object: &GameObject) -> String {
    let tax = format!("{{{}}}", commander::commander_tax(state, object.id));
    let cost = if object.zone == Zone::Command {
        casting::display_spell_cost(state, object.owner, object.id)
    } else {
        None
    };
    match cost
        .map(|cost| mana_cost_text(&cost))
        .filter(|cost| !cost.is_empty())
    {
        Some(cost) => format!("next cast costs {cost}, including commander tax {tax}"),
        None => format!("commander tax {tax} on its next cast from the command zone"),
    }
}

/// CR 903.10a: the combat damage a commander has dealt each player, as the
/// engine tallies it, and how much more from that commander eliminates them.
///
/// `commander_lethal_headroom` answers only in a format that eliminates a
/// player by commander damage, so a format without that rule lists none, as
/// the engine's own display omits it (`derive_views`).
fn commander_damage_lines(
    state: &GameState,
    viewer: PlayerId,
    commander_id: ObjectId,
) -> Vec<String> {
    state
        .commander_damage
        .iter()
        .filter(|entry| entry.commander == commander_id && players::is_alive(state, entry.player))
        .filter_map(|entry| {
            let headroom = commander::commander_lethal_headroom(state, entry.player, commander_id)?;
            let victim = seat_label(entry.player, viewer);
            Some(format!(
                "commander damage dealt to {victim}: {} ({headroom} more eliminates {victim})",
                entry.damage
            ))
        })
        .collect()
}

fn push_stack(out: &mut String, state: &GameState, viewer: PlayerId) {
    if state.stack.is_empty() {
        out.push_str("\n--- STACK ---\n(empty)\n");
        return;
    }
    out.push_str("\n--- STACK (top resolves first) ---\n");
    for (number, entry, name) in stack_items(state) {
        out.push_str(&format!(
            "{number}. {name} (controlled by {})\n",
            seat_label(entry.controller, viewer)
        ));
    }
}

/// The stack section's items, each with its number in the section and the name
/// the section gives it.
///
/// CR 405.2 + CR 405.5: the last object put on the stack is on top and resolves
/// first, so the items are numbered from the top rather than in storage order,
/// and "top" in the prompt means what it means in the rules. An ability on the
/// stack has no card (CR 405.1) and no entry in the object table, so it is
/// named by its source.
fn stack_items(state: &GameState) -> impl Iterator<Item = (usize, &StackEntry, String)> {
    state.stack.iter().rev().enumerate().map(|(index, entry)| {
        let name = object_name(state, entry.id)
            .or_else(|| object_name(state, entry.source_id))
            .unwrap_or_else(|| "unknown".to_string());
        (index + 1, entry, name)
    })
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
        let target = attack_target_name(state, viewer, attacker.attack_target);
        let blockers = combat
            .blocker_assignments
            .get(&attacker.object_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let blocked = blocked_status(state, attacker, blockers);
        out.push_str(&format!("{name} attacking {target} — {blocked}\n"));
    }
}

/// Whether an attacker is blocked, in the combat section's words: `blocked by
/// Grizzly Bears`, `unblocked`, or `blocked (no blockers remain)`.
fn blocked_status(state: &GameState, attacker: &AttackerInfo, blockers: &[ObjectId]) -> String {
    let blockers: Vec<String> = blockers
        .iter()
        .filter_map(|id| object_name(state, *id))
        .collect();
    if !blockers.is_empty() {
        return format!("blocked by {}", blockers.join(", "));
    }
    // CR 509.1h: an attacker stays blocked even with no blockers left.
    if attacker.blocked {
        "blocked (no blockers remain)".to_string()
    } else {
        "unblocked".to_string()
    }
}

/// Who an attack is aimed at, in the combat section's words: a player by seat,
/// a planeswalker or battle by name.
fn attack_target_name(state: &GameState, viewer: PlayerId, target: AttackTarget) -> String {
    match target {
        AttackTarget::Player(player) => seat_label(player, viewer),
        AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => {
            object_name(state, id).unwrap_or_else(|| "unknown".to_string())
        }
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
    // CR 613.1 + CR 613.4c: an object's power and toughness are what the
    // layers settle, its counters and every continuous effect applied, and the
    // lines state those. CR 120.3e + CR 120.6: damage is marked on a creature
    // and checked against its toughness rather than lowering it, so a line
    // states it beside the toughness.
    out.push_str(
        "Power/toughness shown includes counters and continuous effects; \
         damage marked is listed separately.\n",
    );
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
pub(crate) fn mana_symbols(types: impl Iterator<Item = ManaType>) -> String {
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
    parts.extend(permanent_facts(state, object).into_iter().flatten());
    let mut line = parts.join(" | ");
    if let Some(text) = oracle_text(object.name.as_str(), db, options) {
        line.push_str(&format!(" | \"{text}\""));
    }
    line
}

/// How many facts a battlefield line can state between a permanent's name and
/// its rules text: one slot per entry of [`permanent_facts`].
const PERMANENT_FACT_SLOTS: usize = 10;

/// What a battlefield line states about a permanent between its name and its
/// rules text, one slot per fact in the line's order, `None` where the line
/// states nothing.
///
/// Fixed slots rather than a list, so two permanents' facts can be compared
/// fact by fact: [`object_label`] names only the facts on which same-named
/// permanents disagree.
fn permanent_facts(
    state: &GameState,
    object: &GameObject,
) -> [Option<String>; PERMANENT_FACT_SLOTS] {
    let type_line = type_line_text(&object.card_types);
    let counters = counter_text(object);
    let attached: Vec<String> = object
        .attachments
        .iter()
        .filter_map(|attached| object_name(state, *attached))
        .collect();
    let keywords: Vec<String> = object.keywords.iter().map(keyword_text).collect();
    [
        (!type_line.is_empty()).then_some(type_line),
        object
            .power
            .zip(object.toughness)
            .map(|(power, toughness)| format!("{power}/{toughness}")),
        object.loyalty.map(|loyalty| format!("loyalty {loyalty}")),
        Some(if object.tapped { "tapped" } else { "untapped" }.to_string()),
        // CR 302.6: only matters for attacking and {T} abilities, but it is the
        // single most common reason a plausible play is illegal.
        object.summoning_sick.then(|| "summoning sick".to_string()),
        object.face_down.then(|| "face down".to_string()),
        (object.damage_marked > 0).then(|| format!("{} damage marked", object.damage_marked)),
        (!counters.is_empty()).then_some(counters),
        (!attached.is_empty()).then(|| format!("attached: {}", attached.join(", "))),
        (!keywords.is_empty()).then(|| keywords.join(", ")),
    ]
}

/// How an option names one object: its name and whose it is, in the words
/// this board uses, so the model can find it in the position. `None` when the
/// state holds no such object.
///
/// - On the battlefield: `Name (controlled by You)`, as the battlefield lists
///   it under its controller. When the board lists other permanents under the
///   same name and controller, the facts on which they disagree follow, from
///   their battlefield lines and from the combat section (`Name (controlled by
///   Player 1, tapped)`, `Name (controlled by Player 1, blocking Hill Giant)`,
///   `Name (controlled by You, attacking Player 1, unblocked)`). Two labels
///   then read the same only when the permanents' battlefield lines and their
///   places in combat are the same, so nothing the position shows tells them
///   apart.
/// - On the stack: its place in the stack's numbered list and its controller
///   (`Name (stack item 1, controlled by You)`), since that list tells
///   same-named spells apart only by place.
/// - In any other zone: the zone and its owner (`Name (graveyard, owned by
///   You)`), the board's words for exiled cards. Only a permanent or a spell
///   has a controller (CR 108.4).
///
/// `state` is the viewer-filtered state the board is rendered from, so a label
/// can name only what the board shows: a face-down permanent another player
/// controls is `Hidden Card`, as on the battlefield.
pub(crate) fn object_label(state: &GameState, viewer: PlayerId, id: ObjectId) -> Option<String> {
    // The stack is looked up first: an ability on the stack has no entry in the
    // object table, and is named and numbered as the stack section names it.
    if let Some((number, entry, name)) = stack_items(state).find(|(_, entry, _)| entry.id == id) {
        return Some(format!(
            "{name} (stack item {number}, controlled by {})",
            seat_label(entry.controller, viewer)
        ));
    }

    let object = state.objects.get(&id)?;
    let name = one_line(&object.name);
    let owned_in = |zone: &str| format!("{zone}, owned by {}", seat_label(object.owner, viewer));
    let whose = match object.zone {
        Zone::Battlefield => {
            let mut whose = vec![format!(
                "controlled by {}",
                seat_label(object.controller, viewer)
            )];
            whose.extend(distinguishing_facts(state, viewer, object, &name));
            whose.join(", ")
        }
        // A spell object with no entry of its own on the stack.
        Zone::Stack => format!(
            "stack, controlled by {}",
            seat_label(object.controller, viewer)
        ),
        Zone::Graveyard => owned_in("graveyard"),
        Zone::Exile => owned_in("exile"),
        Zone::Hand => owned_in("hand"),
        Zone::Library => owned_in("library"),
        Zone::Command => owned_in("command zone"),
    };
    Some(format!("{name} ({whose})"))
}

/// The facts on which the permanents the battlefield lists under `object`'s
/// name and controller disagree, in the words the board states them: each one's
/// battlefield line and its place in combat.
///
/// Rules text is left out: it is looked up by name, so it never tells
/// same-named permanents apart.
fn distinguishing_facts(
    state: &GameState,
    viewer: PlayerId,
    object: &GameObject,
    name: &str,
) -> Vec<String> {
    let label_facts = |permanent: &GameObject| {
        let mut facts = permanent_facts(state, permanent).to_vec();
        facts.push(combat_role(state, viewer, permanent.id));
        facts
    };
    let namesakes: Vec<Vec<Option<String>>> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|other| other.controller == object.controller && one_line(&other.name) == name)
        .map(label_facts)
        .collect();
    label_facts(object)
        .into_iter()
        .enumerate()
        .filter(|(slot, _)| {
            namesakes
                .iter()
                .any(|facts| facts[*slot] != namesakes[0][*slot])
        })
        .filter_map(|(_, fact)| fact)
        .collect()
}

/// What the combat section says a permanent is doing, in its words: `attacking
/// Player 1, unblocked`, `blocking Hill Giant`. The battlefield line does not
/// say it, so a blocker and a creature beside it, or a blocked attacker and an
/// unblocked one, can share that line.
///
/// Read through the engine's combat-role authority,
/// [`CombatParticipation::capture`].
fn combat_role(state: &GameState, viewer: PlayerId, id: ObjectId) -> Option<String> {
    let participation = CombatParticipation::capture(state, id);
    if let Some(attacker) = participation.attacking {
        return Some(format!(
            "attacking {}, {}",
            attack_target_name(state, viewer, attacker.attack_target),
            blocked_status(state, &attacker, &participation.blocked_by)
        ));
    }
    // CR 509.1g: a blocking creature remains one until it is removed from
    // combat, even once the attackers it blocked have left.
    let blocked: Vec<String> = participation
        .blocking?
        .iter()
        .map(|attacker| object_name(state, *attacker).unwrap_or_else(|| "unknown".to_string()))
        .collect();
    if blocked.is_empty() {
        return Some("blocking".to_string());
    }
    Some(format!("blocking {}", blocked.join(", ")))
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
    let mut line = card_facts(object).join(" | ");
    if let Some(text) = oracle_text(object.name.as_str(), db, options) {
        line.push_str(&format!(" | \"{text}\""));
    }
    line
}

/// What a card off the battlefield is: its name, and its mana cost, type line,
/// and power and toughness where it has them.
fn card_facts(object: &GameObject) -> Vec<String> {
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
    parts
}

/// A permanent's counters by the names cards give them: `1 +1/+1 counter, 8
/// charge counters`.
///
/// Counted by name, since CR 122.1 makes counters with the same name
/// interchangeable: two kinds the engine keeps apart but cards name alike (a
/// `+1/+1` counter and a power-toughness counter of `+1/+1`) are one count.
fn counter_text(object: &GameObject) -> String {
    // `BTreeMap` for a deterministic order: `counters` is a `HashMap`, and an
    // unordered render would make the decision fingerprint unstable.
    let mut by_name: BTreeMap<String, u32> = BTreeMap::new();
    for (kind, count) in &object.counters {
        if *count > 0 {
            // A generic counter's name is card data and may carry a line break.
            *by_name.entry(one_line(&kind.display_phrase())).or_default() += *count;
        }
    }
    by_name
        .into_iter()
        .map(|(name, count)| {
            let noun = if count == 1 { "counter" } else { "counters" };
            format!("{count} {name} {noun}")
        })
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

    /// Issue #10939: an option names a permanent by name and controller, and
    /// when the battlefield lists other permanents under that name and
    /// controller, by the facts on which their lines disagree. Two labels then
    /// read the same only when the battlefield gives the two permanents one
    /// `×N` line.
    #[test]
    fn a_label_tells_same_named_permanents_apart_only_where_the_board_does() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let mut bears = |controller: PlayerId| {
            let id = create_object(
                &mut state,
                CardId(1),
                controller,
                "Grizzly Bears".to_string(),
                Zone::Battlefield,
            );
            let object = state.objects.get_mut(&id).unwrap();
            object.power = Some(2);
            object.toughness = Some(2);
            id
        };
        let mine = bears(PlayerId(0));
        let untapped = bears(PlayerId(1));
        let tapped = bears(PlayerId(1));
        let twin = bears(PlayerId(1));
        let damaged = bears(PlayerId(1));
        state.objects.get_mut(&tapped).unwrap().tapped = true;
        state.objects.get_mut(&damaged).unwrap().damage_marked = 1;

        let viewer = PlayerId(0);
        let visible = filter_state_for_viewer(&state, viewer);
        let label = |id| object_label(&visible, viewer, id).unwrap();
        // Alone under its controller: name and controller say it all.
        assert_eq!(label(mine), "Grizzly Bears (controlled by You)");
        // Tapped and damage tell the opponent's apart; P/T, shared by all of
        // them, is left out.
        assert_eq!(
            label(untapped),
            "Grizzly Bears (controlled by Player 1, untapped)"
        );
        assert_eq!(
            label(tapped),
            "Grizzly Bears (controlled by Player 1, tapped)"
        );
        assert_eq!(
            label(damaged),
            "Grizzly Bears (controlled by Player 1, untapped, 1 damage marked)"
        );
        // Interchangeable: the board itself shows them as one line.
        assert_eq!(label(twin), label(untapped));
        let board = rendered(&state, viewer);
        assert!(
            board.contains("Grizzly Bears | 2/2 | untapped ×2"),
            "{board}"
        );
        assert!(board.contains("Grizzly Bears | 2/2 | tapped\n"), "{board}");
    }

    /// The battlefield line does not say which creature blocks, so a blocker
    /// and an untapped creature beside it share it. Their labels add their
    /// places in combat from the combat section.
    #[test]
    fn a_label_tells_a_blocker_from_the_same_named_creature_beside_it() {
        use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};

        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let mut creature = |controller: PlayerId, name: &str| {
            create_object(
                &mut state,
                CardId(1),
                controller,
                name.to_string(),
                Zone::Battlefield,
            )
        };
        let giant = creature(PlayerId(0), "Hill Giant");
        let blocker = creature(PlayerId(1), "Grizzly Bears");
        let bystander = creature(PlayerId(1), "Grizzly Bears");
        // CR 508.1f + CR 509.1g: Hill Giant attacks Player 1 and one Grizzly
        // Bears blocks it; blocking does not tap it.
        let mut combat = CombatState::default();
        combat.attackers.push(AttackerInfo::new(
            giant,
            AttackTarget::Player(PlayerId(1)),
            PlayerId(1),
        ));
        combat.blocker_assignments.insert(giant, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![giant]);
        state.combat = Some(combat);
        state.objects.get_mut(&giant).unwrap().tapped = true;

        let viewer = PlayerId(0);
        let board = rendered(&state, viewer);
        assert!(board.contains("Grizzly Bears | untapped ×2"), "{board}");
        assert!(board.contains("blocked by Grizzly Bears"), "{board}");
        let visible = filter_state_for_viewer(&state, viewer);
        let label = |id| object_label(&visible, viewer, id).unwrap();
        assert_eq!(
            label(blocker),
            "Grizzly Bears (controlled by Player 1, blocking Hill Giant)"
        );
        assert_eq!(label(bystander), "Grizzly Bears (controlled by Player 1)");
        assert_eq!(
            label(giant),
            "Hill Giant (controlled by You)",
            "alone under its controller, its attack needs no saying"
        );
    }

    /// A label is read from the viewer-filtered state the board is rendered
    /// from, so it names a face-down permanent another player controls only as
    /// the board does.
    #[test]
    fn a_label_never_names_a_face_down_permanent_the_viewer_cannot_see() {
        use engine::game::morph::apply_face_down_creature_characteristics;
        use engine::game::printed_cards::snapshot_object_face;
        use engine::types::ability::FaceDownProfile;

        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let morph = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Akroma, Angel of Fury".to_string(),
            Zone::Battlefield,
        );
        // Turned face down as the engine does it: the CR 708.2a face-down
        // 2/2, with its printed face kept in `back_face`.
        let printed = snapshot_object_face(&state.objects[&morph]);
        let object = state.objects.get_mut(&morph).unwrap();
        apply_face_down_creature_characteristics(object, &FaceDownProfile::vanilla_2_2());
        object.back_face = Some(printed);

        let label =
            |viewer| object_label(&filter_state_for_viewer(&state, viewer), viewer, morph).unwrap();
        assert_eq!(label(PlayerId(0)), "Hidden Card (controlled by Player 1)");
        assert!(rendered(&state, PlayerId(0)).contains("Hidden Card"));
        // Its controller may look at it (CR 708.5), and its label says so.
        assert_eq!(
            label(PlayerId(1)),
            "Akroma, Angel of Fury (controlled by You)"
        );
    }

    /// Issue #10997: battlefield lines printed keywords as Rust debug text
    /// (`Cycling(Mana(Cost { shards: [], generic: 3 }))`, `DoubleStrike`).
    #[test]
    fn battlefield_keywords_read_as_cards_print_them() {
        use engine::types::ability::{ControllerRef, TargetFilter, TypedFilter};
        use engine::types::keywords::{CyclingCost, Keyword, WardCost};
        use engine::types::mana::{ManaCost, ManaCostShard};

        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let mut permanent = |name: &str, keywords: Vec<Keyword>| {
            let id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().keywords = keywords;
        };
        // Macetail Hystrodon: "First strike, haste" and "Cycling {3}".
        permanent(
            "Macetail Hystrodon",
            vec![
                Keyword::FirstStrike,
                Keyword::Haste,
                Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(3))),
            ],
        );
        // Aboleth Spawn: "Flash" and "Ward {2}".
        permanent(
            "Aboleth Spawn",
            vec![
                Keyword::Flash,
                Keyword::Ward(WardCost::Mana(ManaCost::generic(2))),
            ],
        );
        permanent("Arashin Foremost", vec![Keyword::DoubleStrike]);
        // Brink of Disaster: "Enchant creature or land".
        permanent(
            "Brink of Disaster",
            vec![Keyword::Enchant(TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::Typed(TypedFilter::land()),
                ],
            })],
        );
        // Aspect of Lamprey: "Enchant creature you control", which its rules
        // text states; the keyword alone must not read "Enchant creature".
        permanent(
            "Aspect of Lamprey",
            vec![Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))],
        );
        // Steel Seraph: a prototype {1}{W}{W} 3/3 with flying.
        permanent(
            "Steel Seraph",
            vec![
                Keyword::Prototype {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::White, ManaCostShard::White],
                        generic: 1,
                    },
                    power: Some(3),
                    toughness: Some(3),
                },
                Keyword::Flying,
            ],
        );
        // Thought Lash's cumulative upkeep cost is "Exile the top card of your
        // library." Detritivore has suspend X, at a cost of {X}{3}{R}. Both
        // keywords as the card data encodes them; neither can print whole.
        let keyword = |json: serde_json::Value| serde_json::from_value::<Keyword>(json).unwrap();
        permanent(
            "Thought Lash",
            vec![keyword(serde_json::json!({
                "CumulativeUpkeep": {
                    "type": "Exile",
                    "count": 1,
                    "zone": "Library",
                    "filter": null,
                },
            }))],
        );
        permanent(
            "Detritivore",
            vec![keyword(serde_json::json!({
                "Suspend": {
                    "count": 0,
                    "cost": { "type": "Cost", "shards": [], "generic": 0 },
                },
            }))],
        );

        let board = rendered(&state, PlayerId(0));
        for line in [
            "- Macetail Hystrodon | untapped | First strike, Haste, Cycling {3}\n",
            "- Aboleth Spawn | untapped | Flash, Ward {2}\n",
            "- Arashin Foremost | untapped | Double strike\n",
            "- Brink of Disaster | untapped | Enchant creature or land\n",
            "- Aspect of Lamprey | untapped | Enchant\n",
            "- Steel Seraph | untapped | Prototype {1}{W}{W} — 3/3, Flying\n",
            "- Thought Lash | untapped | Cumulative upkeep\n",
            "- Detritivore | untapped | Suspend\n",
        ] {
            assert!(board.contains(line), "missing {line:?} in\n{board}");
        }
        for debug_text in ["Cost {", "{ count", "Some("] {
            assert!(!board.contains(debug_text), "{debug_text:?} in\n{board}");
        }
    }

    /// Issue #10997: counters printed as `1 Plus1Plus1 counter(s)` and
    /// `8 Generic("charge") counter(s)`.
    #[test]
    fn counters_read_by_the_names_cards_give_them() {
        use engine::types::counter::CounterType;

        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        let mut permanent = |name: &str, counters: Vec<(CounterType, u32)>| {
            let id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .counters
                .extend(counters);
        };
        permanent("Grizzly Bears", vec![(CounterType::Plus1Plus1, 1)]);
        // Aether Vial: "At the beginning of your upkeep, you may put a charge
        // counter on this artifact."
        permanent(
            "Aether Vial",
            vec![(CounterType::Generic("charge".to_string()), 8)],
        );
        // CR 122.1: counters with the same name are interchangeable, so two
        // kinds the engine keeps apart that both read "+1/+1" are one count.
        permanent(
            "Steel Overseer",
            vec![
                (CounterType::Plus1Plus1, 2),
                (
                    CounterType::PowerToughness {
                        power: 1,
                        toughness: 1,
                    },
                    1,
                ),
            ],
        );

        let board = rendered(&state, PlayerId(0));
        for line in [
            "- Grizzly Bears | untapped | 1 +1/+1 counter\n",
            "- Aether Vial | untapped | 8 charge counters\n",
            "- Steel Overseer | untapped | 3 +1/+1 counters\n",
        ] {
            assert!(board.contains(line), "missing {line:?} in\n{board}");
        }
    }

    /// Issue #10997: a seat could not tell whether a creature's power and
    /// toughness already include its counters and an anthem, so the
    /// battlefield says it once, for every seat's permanents.
    #[test]
    fn the_battlefield_says_once_what_power_and_toughness_include() {
        let note = "Power/toughness shown includes counters and continuous effects; \
                    damage marked is listed separately.\n";
        let mut state = GameState::new(FormatConfig::standard(), 2, 3);
        for controller in [PlayerId(0), PlayerId(1)] {
            create_object(
                &mut state,
                CardId(1),
                controller,
                "Grizzly Bears".to_string(),
                Zone::Battlefield,
            );
        }
        let board = rendered(&state, PlayerId(0));
        assert_eq!(board.matches(note).count(), 1, "{board}");
        assert!(
            board.contains(&format!("--- BATTLEFIELD ---\n{note}")),
            "{board}"
        );
        // An empty battlefield has no power or toughness to explain.
        let empty = rendered(&GameState::new(FormatConfig::standard(), 2, 3), PlayerId(0));
        assert!(empty.contains("--- BATTLEFIELD ---\n(empty)\n"), "{empty}");
        assert!(!empty.contains("Power/toughness"), "{empty}");
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

    /// The whole COMMANDERS section, ending where the stack section begins.
    fn commanders_section(lines: &[&str]) -> String {
        format!("\n--- COMMANDERS ---\n{}\n\n--- STACK", lines.join("\n"))
    }

    /// Issue #10994: in Commander the position showed no commander in the
    /// command zone, no cost or tax for its next cast, and no commander damage.
    /// A Commander game played through the engine: Player 0 casts its commander
    /// from the command zone, attacks Player 1 with it, then Bolts it and
    /// returns it to the command zone, where its next cast carries {2} of tax.
    /// Player 0's Ruby Medallion takes {1} off each cast, so the stated cost is
    /// the engine's whole cost, not the printed cost plus the tax.
    #[test]
    fn the_position_shows_each_commander_its_next_cast_and_its_commander_damage() {
        use engine::game::scenario::GameScenario;
        use engine::types::actions::GameAction;
        use engine::types::game_state::WaitingFor;
        use engine::types::mana::{ManaCost, ManaCostShard, ManaUnit};
        use engine::types::phase::Phase;

        const KIKI_JIKI: &str = "Haste\n{T}: Create a token that's a copy of target \
             nonlegendary creature you control, except it has haste. Sacrifice it at the \
             beginning of the next end step.";
        const GIADA: &str = "Flying, vigilance\nEach other Angel you control enters with an \
             additional +1/+1 counter on it for each Angel you already control.\n{T}: Add \
             {W}. Spend this mana only to cast an Angel spell.";
        const RUBY_MEDALLION: &str = "Red spells you cast cost {1} less to cast.";

        let owner = PlayerId(0);
        let viewer = PlayerId(1);
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 10994);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_artifact_from_oracle(owner, "Ruby Medallion", RUBY_MEDALLION);
        let kiki = scenario
            .add_creature_to_hand(owner, "Kiki-Jiki, Mirror Breaker", 2, 2)
            .from_oracle_text_with_keywords(&["Haste"], KIKI_JIKI)
            .as_legendary()
            .with_subtypes(vec!["Goblin", "Shaman"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red; 3],
                generic: 2,
            })
            .id();
        scenario.with_commander(kiki);
        let giada = scenario
            .add_creature_to_hand(viewer, "Giada, Font of Hope", 2, 2)
            .from_oracle_text_with_keywords(&["Flying", "Vigilance"], GIADA)
            .as_legendary()
            .with_subtypes(vec!["Angel"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .id();
        scenario.with_commander(giada);
        let bolt = scenario.add_bolt_to_hand(owner);
        // {2}{R}{R}{R}, less the Medallion's {1}.
        let red: Vec<ManaUnit> = (0..4)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, Vec::new()))
            .collect();
        scenario.with_mana_pool(owner, red);
        let mut runner = scenario.build();

        let giada_line = "  - Giada, Font of Hope | {1}{W} | Legendary Creature — Angel | 2/2 \
                          | in the command zone | next cast costs {1}{W}, including commander \
                          tax {0}";
        let kiki_in_command_zone = |cost: &str, tax: &str| {
            format!(
                "  - Kiki-Jiki, Mirror Breaker | {{2}}{{R}}{{R}}{{R}} | Legendary Creature — \
                 Goblin Shaman | 2/2 | in the command zone | next cast costs {cost}, including \
                 commander tax {tax}"
            )
        };

        // Before any cast, both commanders wait in the command zone, untaxed.
        // CR 601.2f: the Medallion reduces Kiki-Jiki, a red spell Player 0
        // casts, and not Giada, which Player 1 casts.
        let kiki_line = kiki_in_command_zone("{1}{R}{R}{R}", "{0}");
        let board = rendered(runner.state(), viewer);
        let expected = commanders_section(&[
            "Player 0 (color identity: {R}):",
            &kiki_line,
            "You (color identity: {W}):",
            giada_line,
        ]);
        assert!(board.contains(&expected), "{board}");

        // CR 903.8: Player 0 casts its commander from the command zone, and
        // it attacks Player 1 the turn it arrives (CR 702.10b: haste).
        runner
            .cast(kiki)
            .resolve()
            .assert_zone(&[kiki], Zone::Battlefield);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(kiki, AttackTarget::Player(viewer))])
            .expect("Kiki-Jiki attacks Player 1");
        runner.combat_damage();
        // Reach: the engine counts the cast and the combat damage.
        let state = runner.state();
        assert_eq!(commander::commander_tax(state, kiki), 2);
        let dealt: Vec<(PlayerId, ObjectId, u32)> = state
            .commander_damage
            .iter()
            .map(|entry| (entry.player, entry.commander, entry.damage))
            .collect();
        assert_eq!(dealt, vec![(viewer, kiki, 2)]);

        let damage_line = "    - commander damage dealt to You: 2 (19 more eliminates You)";
        let board = rendered(state, viewer);
        let expected = commanders_section(&[
            "Player 0 (color identity: {R}):",
            "  - Kiki-Jiki, Mirror Breaker | on the battlefield, controlled by Player 0 | \
             commander tax {2} on its next cast from the command zone",
            damage_line,
            "You (color identity: {W}):",
            giada_line,
        ]);
        assert!(board.contains(&expected), "{board}");

        // Player 0 Bolts its own commander. It dies, and CR 903.9a lets its
        // owner put it back into the command zone.
        let bolted = runner.cast(bolt).target_object(kiki).resolve();
        assert!(
            matches!(
                bolted.final_waiting_for(),
                WaitingFor::CommanderZoneChoice {
                    player,
                    commander_id,
                    current_zone: Zone::Graveyard,
                } if *player == owner && *commander_id == kiki
            ),
            "{:?}",
            bolted.final_waiting_for()
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("Player 0 returns its commander to the command zone");
        assert_eq!(runner.state().objects[&kiki].zone, Zone::Command);

        // Its next cast from there costs {2} more (CR 903.8), still less the
        // Medallion's {1}, and the damage it dealt stays with it (CR 903.10a).
        let kiki_line = kiki_in_command_zone("{3}{R}{R}{R}", "{2}");
        let board = rendered(runner.state(), viewer);
        let expected = commanders_section(&[
            "Player 0 (color identity: {R}):",
            &kiki_line,
            damage_line,
            "You (color identity: {W}):",
            giada_line,
        ]);
        assert!(board.contains(&expected), "{board}");

        // Its owner reads the same commanders under its own seat label.
        let board = rendered(runner.state(), owner);
        let expected = commanders_section(&[
            "You (color identity: {R}):",
            &kiki_line,
            "    - commander damage dealt to Player 1: 2 (19 more eliminates Player 1)",
            "Player 1 (color identity: {W}):",
            giada_line,
        ]);
        assert!(board.contains(&expected), "{board}");
    }

    /// CR 400.2 + CR 401.2 + CR 903.3: a commander stays one in a hidden zone,
    /// but a seat that may not look at the card is told neither its name nor
    /// which hidden zone holds it. Its owner shuffled it into their library
    /// rather than the command zone (CR 903.9b), then a tutor put it into their
    /// hand: another seat's COMMANDERS section reads the same before and after.
    #[test]
    fn a_commander_out_of_sight_is_neither_named_nor_located() {
        use engine::game::zones::move_to_zone;
        use engine::types::mana::{ManaCost, ManaCostShard};

        let owner = PlayerId(0);
        let other = PlayerId(1);
        let mut state = GameState::new(FormatConfig::commander(), 2, 3);
        let id = create_object(
            &mut state,
            CardId(1),
            owner,
            "Kenrith, the Returned King".to_string(),
            Zone::Library,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.is_commander = true;
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 4,
        };
        object.mana_cost = cost.clone();
        object.base_mana_cost = cost;

        // No deck pool registers it, so the engine would read its identity
        // from its printed colors and cost, which the filter redacts.
        let hidden = commanders_section(&[
            "Player 0:",
            "  - Hidden Card | face down or in a hidden zone | commander tax {0} on its next \
             cast from the command zone",
        ]);
        let in_library = rendered(&state, other);
        assert!(in_library.contains(&hidden), "{in_library}");

        move_to_zone(&mut state, id, Zone::Hand, &mut Vec::new());
        assert_eq!(state.objects[&id].zone, Zone::Hand);
        let in_hand = rendered(&state, other);
        assert!(in_hand.contains(&hidden), "{in_hand}");
        assert!(
            !in_hand.contains("Kenrith"),
            "hidden card leaked: {in_hand}"
        );

        // Its owner sees the card in their own hand.
        let own = rendered(&state, owner);
        let expected = commanders_section(&[
            "You (color identity: {W}):",
            "  - Kenrith, the Returned King | {4}{W} | in hand | commander tax {0} on its \
             next cast from the command zone",
        ]);
        assert!(own.contains(&expected), "{own}");
    }

    /// CR 903.4 + CR 903.4a: a seat's color identity is the one registered for
    /// its commanders before the game began, rules-text mana symbols included,
    /// and it is public even while the commander is out of sight. Kenrith, the
    /// Returned King costs {4}{W}, and its activated abilities' costs contain
    /// {R}, {G}, {U} and {B}.
    #[test]
    fn a_seats_color_identity_is_the_registered_one() {
        use engine::game::deck_loading::DeckEntry;
        use engine::types::card::CardFace;
        use engine::types::game_state::PlayerDeckPool;
        use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
        use std::sync::Arc;

        let owner = PlayerId(0);
        let mut state = GameState::new(FormatConfig::commander(), 2, 3);
        let id = create_object(
            &mut state,
            CardId(1),
            owner,
            "Kenrith, the Returned King".to_string(),
            Zone::Hand,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.is_commander = true;
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 4,
        };
        object.mana_cost = cost.clone();
        object.base_mana_cost = cost;
        state.deck_pools.push(PlayerDeckPool {
            player: owner,
            current_commander: Arc::new(vec![DeckEntry {
                card: CardFace {
                    name: "Kenrith, the Returned King".to_string(),
                    color_identity: vec![
                        ManaColor::Black,
                        ManaColor::Green,
                        ManaColor::Red,
                        ManaColor::Blue,
                        ManaColor::White,
                    ],
                    ..Default::default()
                },
                count: 1,
            }]),
            ..Default::default()
        });

        // Its owner, who sees the card, and another seat, who does not.
        for (viewer, label, line) in [
            (
                owner,
                "You",
                "  - Kenrith, the Returned King | {4}{W} | in hand | commander tax {0} on its \
                 next cast from the command zone",
            ),
            (
                PlayerId(1),
                "Player 0",
                "  - Hidden Card | face down or in a hidden zone | commander tax {0} on its \
                 next cast from the command zone",
            ),
        ] {
            let board = rendered(&state, viewer);
            let header = format!("{label} (color identity: {{W}}{{U}}{{B}}{{R}}{{G}}):");
            let expected = commanders_section(&[&header, line]);
            assert!(board.contains(&expected), "{board}");
        }
    }

    /// CR 800.4a: a player who has left the game takes their cards with them,
    /// so neither their commanders nor commander damage dealt to them is listed.
    #[test]
    fn a_seat_that_left_the_game_is_not_listed() {
        use engine::game::elimination::eliminate_player;
        use engine::types::events::LossReason;
        use engine::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 3, 3);
        let mut commander = |owner: PlayerId, name: &str| {
            let id = create_object(
                &mut state,
                CardId(1),
                owner,
                name.to_string(),
                Zone::Command,
            );
            state.objects.get_mut(&id).unwrap().is_commander = true;
            id
        };
        let kiki = commander(PlayerId(0), "Kiki-Jiki, Mirror Breaker");
        let giada = commander(PlayerId(2), "Giada, Font of Hope");
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(2),
            commander: kiki,
            damage: 2,
        });
        let section = |board: &str| -> String {
            let (_, rest) = board
                .split_once("--- COMMANDERS ---\n")
                .expect("the position lists commanders");
            let (section, _) = rest.split_once("\n--- STACK").expect("the stack follows");
            section.to_string()
        };

        // Reach: while Player 2 plays, its commander and the damage dealt to
        // it are listed.
        let playing = section(&rendered(&state, PlayerId(1)));
        assert!(playing.contains("Giada, Font of Hope"), "{playing}");
        assert!(
            playing.contains("commander damage dealt to Player 2: 2 (19 more eliminates Player 2)"),
            "{playing}"
        );

        eliminate_player(
            &mut state,
            PlayerId(2),
            LossReason::LifeTotal,
            &mut Vec::new(),
        );
        assert!(!players::is_alive(&state, PlayerId(2)));
        // Reach: its commander leaves the game with it (CR 800.4a), which the
        // engine models as a move to exile. It stays a commander in a public
        // zone there, so only the seat's departure keeps it off the list.
        let departed = &state.objects[&giada];
        assert!(departed.is_commander && departed.zone == Zone::Exile);
        let left = section(&rendered(&state, PlayerId(1)));
        assert!(left.contains("Kiki-Jiki, Mirror Breaker"), "{left}");
        assert!(!left.contains("Giada"), "{left}");
        assert!(!left.contains("Player 2"), "{left}");
    }

    /// The section is only for a format with a command zone, and only once a
    /// commander is in the game.
    #[test]
    fn only_a_game_with_commanders_lists_them() {
        for (format, listed) in [
            (FormatConfig::commander(), true),
            (FormatConfig::standard(), false),
        ] {
            let mut state = GameState::new(format, 2, 3);
            let id = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Kiki-Jiki, Mirror Breaker".to_string(),
                Zone::Command,
            );
            state.objects.get_mut(&id).unwrap().is_commander = true;
            let board = rendered(&state, PlayerId(1));
            assert_eq!(board.contains("--- COMMANDERS ---"), listed, "{board}");
        }
        let without = rendered(
            &GameState::new(FormatConfig::commander(), 2, 3),
            PlayerId(1),
        );
        assert!(!without.contains("--- COMMANDERS ---"), "{without}");
    }
}

//! CR 732.1b: a taken loop shortcut keeps a resolved-rules journal whose size does not grow with
//! the per-repetition payloads of its action-scoped commands (#10475).
//!
//! The board is the report's: Sprout Swarm ("Convoke … Buyback {3} … Create a 1/1 green
//! Saproling creature token."), Witherbloom, the Balancer ("Affinity for creatures … Instant and
//! sorcery spells you cast have affinity for creatures.") and Altar of the Brood ("Whenever
//! another permanent you control enters, each opponent mills a card."). Each repetition pushes
//! and pops Sprout Swarm and the Altar triggers, collects those triggers, and mills every
//! opponent, so it records stack, trigger-collection and zone-change commands that each embed a
//! full stack-entry or object snapshot. Those commands are read only inside the action that
//! recorded them, so each committed repetition retires them; the entries read after the action
//! (node slots, mana, ledger edits, reveals, delayed installs, a leave's sweep) remain.

use engine::game::engine::apply;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    JournalRetention, ResolvedRulesCommand, ResolvedRulesJournal,
};

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;

use super::wba_loop_firewall_interposition::{mill_base, offer_state};

const P0: PlayerId = PlayerId(0);

/// P0 declares `Fixed(n)`; every living opponent accepts in APNAP order.
fn declare_and_accept_all(state: &mut GameState, n: u32) {
    apply(
        state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        },
    )
    .expect("the proposer declares the shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

/// The report's board at its offer, and the board after taking it `n` times.
fn take(n: u32) -> (GameState, GameState) {
    let offer = offer_state(mill_base());
    let mut taken = offer.clone();
    declare_and_accept_all(&mut taken, n);
    assert!(
        taken
            .pending_unbounded_materialization
            .get(&P0)
            .is_none_or(Vec::is_empty),
        "reach: the mill routes the take to the replay, which performs every repetition"
    );
    assert!(
        matches!(taken.waiting_for, WaitingFor::Priority { .. }),
        "reach: the take ends where a player has priority, got {:?}",
        taken.waiting_for
    );
    (offer, taken)
}

fn opponent_library_total(state: &GameState) -> usize {
    state
        .players
        .iter()
        .filter(|player| player.id != P0)
        .map(|player| player.library.len())
        .sum()
}

fn journal_bytes(journal: &ResolvedRulesJournal) -> usize {
    serde_json::to_vec(journal)
        .expect("the journal serializes")
        .len()
}

fn is_snapshot_command(command: &ResolvedRulesCommand) -> bool {
    matches!(
        command,
        ResolvedRulesCommand::TriggerCollection(_)
            | ResolvedRulesCommand::StackPush(_)
            | ResolvedRulesCommand::StackRemoval(_)
            | ResolvedRulesCommand::ZoneChange(_)
    )
}

/// CR 732.1b: every committed repetition retires its action-scoped entries, so none survives the
/// take, the retired ordinals grow with the count, and what each repetition keeps is a small
/// fraction of the snapshot commands the same cycle records when it is performed as an action.
#[test]
fn a_taken_mill_loop_retires_each_repetitions_action_scoped_commands() {
    const SMALL: u32 = 2;
    const LARGE: u32 = 8;

    let (offer, small) = take(SMALL);
    let (_, large) = take(LARGE);

    // Reach: the offer was reached by performing one cycle as an ordinary action, which records
    // and keeps the snapshot commands a repetition would otherwise accumulate.
    let cycle_snapshot_bytes: usize = offer
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .filter(|command| is_snapshot_command(command))
        .map(|command| serde_json::to_vec(command).expect("serializes").len())
        .sum();
    for kind in [
        "TriggerCollection",
        "StackPush",
        "StackRemoval",
        "ZoneChange",
    ] {
        assert!(
            offer
                .resolved_rules_journal
                .entries()
                .iter()
                .filter_map(|entry| entry.command.as_ref())
                .any(
                    |command| serde_json::to_value(command).expect("serializes")[kind].is_object()
                ),
            "reach: an ordinary cycle on this board records a {kind} command"
        );
    }

    for (n, taken) in [(SMALL, &small), (LARGE, &large)] {
        assert!(
            opponent_library_total(taken) < opponent_library_total(&offer),
            "reach: the take at {n} milled the opponents"
        );
        let start = offer.resolved_rules_journal.entries().len();
        let journal = &taken.resolved_rules_journal;
        assert!(
            journal
                .entries()
                .skip(start)
                .iter()
                .all(|entry| entry.retention() == JournalRetention::Turn),
            "the take at {n} keeps no action-scoped entry"
        );
        assert_eq!(
            journal.entries().take(start),
            offer.resolved_rules_journal.entries().take(start),
            "the take at {n} leaves the journal before it untouched"
        );
    }

    let retired = |state: &GameState| {
        let journal = &state.resolved_rules_journal;
        journal.next_command_ordinal().0 as usize - journal.entries().len()
    };
    assert!(
        retired(&large) > retired(&small) && retired(&small) > 0,
        "the retired ordinals grow with the count ({} at {SMALL}, {} at {LARGE})",
        retired(&small),
        retired(&large)
    );

    let per_repetition = (journal_bytes(&large.resolved_rules_journal)
        - journal_bytes(&small.resolved_rules_journal))
        / (LARGE - SMALL) as usize;
    assert!(
        per_repetition * 4 < cycle_snapshot_bytes,
        "a repetition keeps {per_repetition} journal bytes against the {cycle_snapshot_bytes} \
         bytes of snapshot commands an ordinary cycle records"
    );
}

/// The journal a take leaves reloads through the production restore, whose load validator
/// admits the ordinal gaps the retired entries leave, and the reload is the same journal.
#[test]
fn a_taken_loop_journal_reloads_through_the_restore() {
    let (_, taken) = take(3);
    let saved = serde_json::to_value(PersistedGameState::capture(taken.clone()))
        .expect("serialize the authoritative game");
    let restored = serde_json::from_value::<PersistedGameState>(saved)
        .expect("decode the authoritative game")
        .into_game_state()
        .expect("the retired journal satisfies the checked restore contract");
    assert_eq!(
        restored.resolved_rules_journal, taken.resolved_rules_journal,
        "the reload derives the same journal and index"
    );
}

/// CR 800.4a: a take whose repetitions make every opponent lose (Exquisite Blood, "Whenever an
/// opponent loses life, you gain that much life.", with Enduring Tenacity, "Whenever you gain
/// life, target opponent loses that much life.", in a four-player game) still retires its
/// repetitions' action-scoped entries, but keeps each leave and every command of its sweep, so
/// `records_player_leave_move` still finds the sweep's zone changes.
#[test]
fn a_take_that_eliminates_players_keeps_each_leaves_sweep() {
    use engine::types::resolved_commands::RulesExecutionNodeRef;

    let offer = super::three_opponent_drain_shortcut::load_fc();
    let taken = super::three_opponent_drain_shortcut::accept_suggested(&offer);
    assert_eq!(
        taken.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "reach: the take eliminates every opponent"
    );
    let start = offer.resolved_rules_journal.entries().len();
    let journal = &taken.resolved_rules_journal;
    let take_entries = journal.entries().skip(start);
    assert!(
        journal.next_command_ordinal().0 as usize - journal.entries().len()
            > offer.resolved_rules_journal.next_command_ordinal().0 as usize
                - offer.resolved_rules_journal.entries().len(),
        "reach: the take retired its repetitions' action-scoped entries"
    );
    assert!(
        take_entries
            .iter()
            .all(|entry| entry.retention() == JournalRetention::Turn),
        "no action-scoped entry survives the take"
    );

    let leaves: Vec<_> = take_entries
        .iter()
        .filter(|entry| matches!(entry.command, Some(ResolvedRulesCommand::PlayerLeave(_))))
        .collect();
    assert_eq!(leaves.len(), 3, "each opponent's leave is kept");
    let sweep_moves: Vec<_> = take_entries
        .iter()
        .filter(|entry| matches!(entry.node, RulesExecutionNodeRef::PlayerLeave(_)))
        .filter_map(|entry| match entry.command.as_ref() {
            Some(ResolvedRulesCommand::ZoneChange(change)) => Some(change),
            _ => None,
        })
        .collect();
    assert!(
        !sweep_moves.is_empty(),
        "reach: the leaves' sweeps moved the departing players' objects"
    );
    for change in sweep_moves {
        assert!(
            journal.records_player_leave_move(&engine::types::resolved_commands::PlayerLeaveMove {
                object_id: change.object.object_id,
                from: change.from,
                to: change.to,
                turn_zone_change_index: change.turn_zone_change_index,
            }),
            "CR 800.4a: the kept sweep move of {:?} is still found as a leave's",
            change.object
        );
    }
}

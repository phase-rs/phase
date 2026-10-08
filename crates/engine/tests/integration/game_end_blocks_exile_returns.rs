//! CR 104.1 + CR 610.3: a recorded terminal result stops the post-action
//! pipeline's "until this leaves" exile-return pass.
//!
//! `run_post_action_pipeline` calls `check_exile_returns` after its CR 704.3
//! SBA loop. That routine reads the whole action's `ZoneChanged` batch, so a
//! battlefield departure sitting in the batch makes it move the linked exiled
//! card back through the zone-change pipeline — including the departure that is
//! part of ENDING the game.
//!
//! The guard reads `GameState::game_end`, this branch's single terminal-result
//! writer (`elimination::end_game`), rather than `WaitingFor::GameOver`: a
//! later step of the same action can overwrite the wait — a CR 616.1
//! replacement-order prompt raised on the resolving spell's own zone move, for
//! instance — and only `engine::reconcile_terminal_result` restores it at the
//! action boundary. The record is the fact that stands for the whole action.
//!
//! The two rows below separate the two guards. The first reaches the pass
//! with `waiting_for` still `GameOver`, so a `waiting_for`-only guard would
//! pass it too; the second reaches it with the wait overwritten by a CR 616.1
//! replacement-order prompt, where only the `game_end` record still holds.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WHITE_AURACITE: &str = "When this artifact enters, exile target nonland permanent an opponent controls until this artifact leaves the battlefield.\n{T}: Add {W}.";
const FLAME_RIFT: &str = "Flame Rift deals 4 damage to each player.";

/// CR 104.1 + CR 800.4a: the CR 800.4a sweep that ends the game must not then
/// hand its own `ZoneChanged` batch back to the exile-return pass.
///
/// Flame Rift is exactly lethal for its caster, so the next SBA check loses P0
/// (CR 704.5a) and P1 wins (CR 104.2a). Eliminating P0 first sweeps every
/// object P0 owns off the battlefield (CR 800.4a) — White Auracite among them —
/// and only after that does `check_game_over` record the result and emit the
/// one `GameOver` event. White Auracite's battlefield departure is therefore in
/// the batch, ahead of `GameOver`, when the pipeline reaches the exile-return
/// pass, and P1 is still in the game, so the link it would spend is still live.
///
/// Revert the CR 104.1 guard at the `check_exile_returns` call and P1's exiled
/// creature is put back onto the battlefield of a finished game.
#[test]
fn terminal_result_runs_no_exile_return_after_the_game_ending_sweep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 704.5a: exactly lethal for the caster only, so the result is a win for
    // P1 and P1's own exiled card is never swept by CR 800.4a.
    scenario.with_life(P0, 4);
    scenario.with_life(P1, 20);

    let auracite = {
        let mut auracite = scenario.add_creature_to_hand(P0, "White Auracite", 0, 0);
        auracite.as_artifact().from_oracle_text(WHITE_AURACITE);
        auracite.id()
    };
    let exiled = scenario.add_creature(P1, "Exiled Creature", 2, 2).id();
    let rift = scenario
        .add_spell_to_hand_from_oracle(P0, "Flame Rift", false, FLAME_RIFT)
        .id();

    let mut runner = scenario.build();

    // Setup, through the cast pipeline: White Auracite's ETB exiles the
    // opponent's creature and installs the `UntilSourceLeaves` link whose
    // return the game-ending departure would otherwise owe (CR 610.3).
    runner.cast(auracite).target_object(exiled).resolve();
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "reach-guard: White Auracite's ETB must have exiled the creature"
    );
    assert_eq!(
        runner.state().objects[&auracite].zone,
        Zone::Battlefield,
        "reach-guard: the exile link's source must be on the battlefield"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == auracite && link.exiled_id == exiled),
        "reach-guard: the linked exile must be live before the game ends"
    );

    let outcome = runner.cast(rift).resolve();

    let events = outcome.events();
    let game_over_index = events
        .iter()
        .position(|event| matches!(event, GameEvent::GameOver { .. }))
        .expect("Flame Rift must end the game");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .count(),
        1,
        "CR 104.1: the result must be announced exactly once"
    );
    // Reach-guard for the negative assertions below: the source's battlefield
    // departure really is in the batch `check_exile_returns` reads, so an
    // unguarded pass WOULD move the exiled card. Without this the "nothing
    // moved" assertions could pass for the wrong reason — no matching event at
    // all.
    assert!(
        events[..game_over_index].iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                ..
            } if *object_id == auracite
        )),
        "reach-guard: CR 800.4a must sweep White Auracite off the battlefield \
         before the result is recorded, events = {events:?}"
    );

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: Some(winner) } if *winner == P1
        ),
        "CR 104.2a: the surviving opponent wins, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.state().objects.get(&exiled).map(|obj| obj.zone),
        Some(Zone::Exile),
        "CR 104.1: the exiled card must not be returned after the game ended"
    );
    assert!(
        outcome
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == auracite && link.exiled_id == exiled),
        "CR 104.1: the exile-return pass must not run at all once the result is \
         recorded, so its link is neither spent nor dropped"
    );
    let post_game_zone_changes: Vec<&GameEvent> = events[game_over_index + 1..]
        .iter()
        .filter(|event| matches!(event, GameEvent::ZoneChanged { .. }))
        .collect();
    assert!(
        post_game_zone_changes.is_empty(),
        "CR 104.1: no zone change may be appended after the game ended, got \
         {post_game_zone_changes:?}"
    );
}

const LABORATORY_MANIAC: &str =
    "If you would draw a card while your library has no cards in it, you win the game instead.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const OPT: &str = "Scry 1.\nDraw a card.";

/// CR 104.1 + CR 616.1: the same guard, reached with `WaitingFor` OVERWRITTEN.
///
/// The row above reaches the exile-return pass with `waiting_for` still
/// `GameOver`, so it cannot tell the shipped `state.game_end.is_none()` guard
/// apart from a weaker `!matches!(state.waiting_for, GameOver { .. })` one.
/// This row separates them: at the pass, the recorded result stands but the
/// wait is a CR 616.1 replacement-order prompt.
///
/// P1's Laboratory Maniac replaces Opt's draw with "you win the game"
/// (CR 614.6 + CR 104.2b), which eliminates P0 mid-resolution (CR 104.3e).
/// CR 800.4a exiles everything P0 still controlled — White Auracite, whose
/// `UntilSourceLeaves` link is still owed to P1's exiled creature — and only
/// then does the result get recorded. Resolution then continues: CR 608.2n
/// puts Opt into P1's graveyard, P1's two Rest in Peace both want to
/// exile it instead, and CR 616.1 makes that order P1's choice. Installing that
/// prompt overwrites `WaitingFor::GameOver`; `engine::reconcile_terminal_result`
/// restores it only at the action boundary, which is why `game_end` — not
/// `waiting_for` — is the guard's authority.
///
/// Swap the guard for `!matches!(state.waiting_for, GameOver { .. })` and the
/// exiled creature is put back onto the battlefield of a finished game.
#[test]
fn terminal_result_runs_no_exile_return_while_a_replacement_prompt_owns_the_wait() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let auracite = {
        let mut auracite = scenario.add_creature_to_hand(P0, "White Auracite", 0, 0);
        auracite.as_artifact().from_oracle_text(WHITE_AURACITE);
        auracite.id()
    };
    let exiled = scenario.add_creature(P1, "Exiled Creature", 2, 2).id();
    // CR 614.6 + CR 104.2b: the game-ender. P1's library is empty (the scenario
    // builder's default), so Opt's draw is fully replaced by the win.
    scenario
        .add_creature(P1, "Laboratory Maniac", 2, 2)
        .from_oracle_text(LABORATORY_MANIAC);
    // CR 616.1: two applicable replacements on the SAME graveyard move are what
    // makes the order a choice — one Rest in Peace would apply silently.
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let opt = scenario
        .add_spell_to_hand_from_oracle(P1, "Opt", true, OPT)
        .id();

    let mut runner = scenario.build();
    assert!(
        runner.state().players[P1.0 as usize].library.is_empty(),
        "reach-guard: Laboratory Maniac's replacement is gated on an empty library"
    );

    // Setup, through the cast pipeline: the `UntilSourceLeaves` link whose
    // return the game-ending sweep would otherwise owe (CR 610.3).
    runner.cast(auracite).target_object(exiled).resolve();
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "reach-guard: White Auracite's ETB must have exiled the creature"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == auracite && link.exiled_id == exiled),
        "reach-guard: the linked exile must be live before the game ends"
    );

    // CR 117.1a: Opt is P1's instant, so P1 must hold priority to cast it.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 must be able to pass priority in its own main phase");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "reach-guard: P1 must hold priority, got {:?}",
        runner.state().waiting_for
    );

    let outcome = runner.cast(opt).resolve();

    let events = outcome.events();
    let game_over_index = events
        .iter()
        .position(|event| matches!(event, GameEvent::GameOver { .. }))
        .expect("the replaced draw must win the game for P1");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .count(),
        1,
        "CR 104.1: the result must be announced exactly once"
    );
    // Reach-guard for the negative assertions below: the link's source really
    // did leave the battlefield inside the batch `check_exile_returns` reads,
    // so an unguarded pass WOULD move the exiled card back.
    assert!(
        events[..game_over_index].iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                ..
            } if *object_id == auracite
        )),
        "reach-guard: CR 800.4a must sweep White Auracite off the battlefield \
         before the result is recorded, events = {events:?}"
    );
    // THE discriminating reach-guard: without a parked CR 616.1 order choice
    // the pass would see `waiting_for == GameOver` and a `waiting_for`-only
    // guard would block the return for the wrong reason — exactly the blind
    // spot the sibling row above has.
    assert!(
        outcome.state().pending_replacement.is_some(),
        "reach-guard: P1's two Rest in Peace must have parked a CR 616.1 order \
         choice on Opt's graveyard move, overwriting the GameOver wait"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: Some(winner) } if *winner == P1
        ),
        "CR 104.1: `reconcile_terminal_result` restores the terminal wait at the \
         action boundary, got {:?}",
        outcome.final_waiting_for()
    );

    assert_eq!(
        outcome.state().objects.get(&exiled).map(|obj| obj.zone),
        Some(Zone::Exile),
        "CR 104.1: the exiled card must not be returned after the game ended"
    );
    assert!(
        outcome
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == auracite && link.exiled_id == exiled),
        "CR 104.1: the exile-return pass must not run at all once the result is \
         recorded, so its link is neither spent nor dropped"
    );
    let exiled_zone_changes: Vec<&GameEvent> = events
        .iter()
        .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == exiled))
        .collect();
    assert!(
        exiled_zone_changes.is_empty(),
        "CR 104.1: the exiled card may not move at all once the result is \
         recorded, got {exiled_zone_changes:?}"
    );
    let post_game_zone_changes: Vec<&GameEvent> = events[game_over_index + 1..]
        .iter()
        .filter(|event| matches!(event, GameEvent::ZoneChanged { .. }))
        .collect();
    assert!(
        post_game_zone_changes.is_empty(),
        "CR 104.1: no zone change may be appended after the game ended, got \
         {post_game_zone_changes:?}"
    );
}

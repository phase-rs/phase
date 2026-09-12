//! CR 104.1 + CR 610.3a: a recorded terminal result stops the post-action
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

use engine::game::scenario::{GameScenario, P0, P1};
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

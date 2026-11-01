//! Timesifter: "At the beginning of each upkeep, each player exiles the top
//! card of their library. The player who exiled the card with the greatest
//! mana value takes an extra turn after this one. If two or more players'
//! cards are tied for greatest, the tied players repeat this process until the
//! tie is broken."
//!
//! Drives the real upkeep trigger and checks who receives the extra turn
//! (CR 500.7) and which cards each round exiled (CR 701.13a). Mana values are
//! read from the exiled cards (CR 202.3).

use engine::game::scenario::GameRunner;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectKind, QuantityExpr, ReplacementDefinition,
    TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExtraTurn, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

const TIMESIFTER: &str = "At the beginning of each upkeep, each player exiles the top card of \
    their library. The player who exiled the card with the greatest mana value takes an extra \
    turn after this one. If two or more players' cards are tied for greatest, the tied players \
    repeat this process until the tie is broken.";

/// Stages `player`'s library top-first with cards of the given mana values,
/// above two filler cards so the draw step cannot deck anyone.
fn stack_library(
    scenario: &mut GameScenario,
    player: PlayerId,
    mana_values: &[u32],
) -> Vec<ObjectId> {
    scenario.with_library_top(player, &["Filler", "Filler"]);
    let mut ids: Vec<ObjectId> = mana_values
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &mv)| {
            scenario
                .add_spell_to_library_top(player, &format!("P{} card {i}", player.0), false)
                .with_mana_cost(ManaCost::generic(mv))
                .id()
        })
        .collect();
    ids.reverse();
    ids
}

/// Runs P0's upkeep with Timesifter on the battlefield and returns the final
/// extra-turn queue plus a zone lookup for the staged cards.
fn run_upkeep(p0_values: &[u32], p1_values: &[u32]) -> (Vec<ExtraTurn>, Vec<Zone>, Vec<Zone>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_artifact_from_oracle(P0, "Timesifter", TIMESIFTER);
    let p0_cards = stack_library(&mut scenario, P0, p0_values);
    let p1_cards = stack_library(&mut scenario, P1, p1_values);

    let mut runner = scenario.build();
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let zones = |ids: &[ObjectId]| ids.iter().map(|id| state.objects[id].zone).collect();
    (
        state.extra_turns.clone(),
        zones(&p0_cards),
        zones(&p1_cards),
    )
}

#[test]
fn greatest_mana_value_takes_an_extra_turn_after_this_one() {
    let (extra_turns, p0_zones, p1_zones) = run_upkeep(&[2, 9], &[5, 0]);

    // CR 500.7: P1's extra turn comes directly after P0's current turn.
    assert_eq!(
        extra_turns,
        vec![ExtraTurn {
            player: P1,
            anchor: P0,
        }]
    );
    // One round: only the top cards were exiled.
    assert_eq!(p0_zones[0], Zone::Exile);
    assert_eq!(p1_zones[0], Zone::Exile);
    assert_ne!(p0_zones[1], Zone::Exile);
    assert_ne!(p1_zones[1], Zone::Exile);
}

#[test]
fn tied_players_repeat_until_the_tie_is_broken() {
    let (extra_turns, p0_zones, p1_zones) = run_upkeep(&[3, 1, 4], &[3, 1, 2]);

    // Rounds one and two tie (3 vs 3, 1 vs 1); round three breaks it for P0.
    assert_eq!(
        extra_turns,
        vec![ExtraTurn {
            player: P0,
            anchor: P0,
        }]
    );
    assert_eq!(p0_zones, vec![Zone::Exile; 3]);
    assert_eq!(p1_zones, vec![Zone::Exile; 3]);
}

#[test]
fn each_repeated_round_is_its_own_simultaneous_exile() {
    // CR 608.2e + CR 603.2c: each round's exiles happen simultaneously, so a
    // batched "one or more cards are put into exile" observer with no origin or
    // owner restriction triggers once per round — three times for a contest
    // decided in its third round. Splitting a round into per-card moves would
    // trigger it six times.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_artifact_from_oracle(P0, "Timesifter", TIMESIFTER);
    let mut exile_observer = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
        .destination(Zone::Exile)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));
    exile_observer.batched = true;
    scenario
        .add_creature(P0, "Exile Watcher", 0, 1)
        .with_trigger_definition(exile_observer);
    stack_library(&mut scenario, P0, &[3, 1, 4]);
    stack_library(&mut scenario, P1, &[3, 1, 2]);

    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), life_before + 3);
    assert_eq!(
        runner.state().extra_turns,
        vec![ExtraTurn {
            player: P0,
            anchor: P0,
        }]
    );
}

/// A replacement that offers its affected player a CR 616.1 choice whenever
/// `card` would be exiled, sending it to the graveyard instead.
fn graveyard_redirect(card: ObjectId) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Graveyard,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .destination_zone(Zone::Exile)
        .valid_card(TargetFilter::SpecificObject { id: card })
}

/// The staged cards of the replacement-pause fixture.
struct PausedContest {
    p0_cards: Vec<ObjectId>,
    p1_cards: Vec<ObjectId>,
    p2_cards: Vec<ObjectId>,
}

/// CR 616.1 + CR 608.2c: P0's first card meets two applicable replacements,
/// pausing the round before P1 and P2 exile. Returns the runner parked on that
/// replacement choice.
fn pause_contest_on_replacement_choice() -> (GameRunner, PausedContest) {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Untap);
    scenario.add_artifact_from_oracle(P0, "Timesifter", TIMESIFTER);
    let p0_cards = stack_library(&mut scenario, P0, &[9, 0]);
    let p1_cards = stack_library(&mut scenario, P1, &[3, 4]);
    let p2_cards = stack_library(&mut scenario, p2, &[3, 2]);
    scenario
        .add_creature(P0, "Redirector", 1, 1)
        .with_replacement_definition(graveyard_redirect(p0_cards[0]))
        .with_replacement_definition(graveyard_redirect(p0_cards[0]));

    let mut runner = scenario.build();
    runner.auto_advance_to_main_phase();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the redirected exile must pause for the replacement choice"
    );
    (
        runner,
        PausedContest {
            p0_cards,
            p1_cards,
            p2_cards,
        },
    )
}

/// Answers every pending replacement choice, finishes the upkeep, and checks
/// that the round finished, P1 and P2 tied, and the repeat round broke the tie
/// for P1.
fn finish_paused_contest(mut runner: GameRunner, cards: &PausedContest) {
    let mut events = Vec::new();
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .unwrap();
                events.extend(result.events);
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    let state = runner.state();
    let zone = |id: &ObjectId| state.objects[id].zone;
    assert_eq!(
        state.extra_turns,
        vec![ExtraTurn {
            player: P1,
            anchor: P0,
        }]
    );
    // P0's card was not exiled, so P0 dropped out after round one.
    assert_eq!(zone(&cards.p0_cards[0]), Zone::Graveyard);
    assert_eq!(zone(&cards.p0_cards[1]), Zone::Library);
    assert!(cards.p1_cards.iter().all(|id| zone(id) == Zone::Exile));
    assert!(cards.p2_cards.iter().all(|id| zone(id) == Zone::Exile));
    let contest_resolutions = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ExileTopCardContest,
                    ..
                }
            )
        })
        .count();
    assert_eq!(contest_resolutions, 1);
}

#[test]
fn a_replacement_choice_resumes_the_contest() {
    let (runner, cards) = pause_contest_on_replacement_choice();
    finish_paused_contest(runner, &cards);
}

#[test]
fn a_paused_contest_survives_a_serialized_state_round_trip() {
    // The paused state carries both new wire shapes: the contest effect in
    // Timesifter's ability definitions and the parked round completion. A
    // state restored from its JSON form must finish the contest exactly as the
    // live state does.
    let (runner, cards) = pause_contest_on_replacement_choice();
    let json = serde_json::to_string(runner.state()).expect("paused contest state serializes");
    assert!(json.contains("\"ExileTopCardContest\""));
    assert!(json.contains("\"TopCardContestRoundComplete\""));

    let restored: GameState =
        serde_json::from_str(&json).expect("paused contest state deserializes");
    assert_eq!(
        serde_json::to_value(&restored).expect("restored state serializes"),
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        "a serialized round trip must be lossless"
    );
    finish_paused_contest(GameRunner::from_state(restored), &cards);
}

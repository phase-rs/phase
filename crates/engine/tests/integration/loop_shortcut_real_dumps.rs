//! CR 732.2a: the Niv-Mizzet turn-44 and Magda turn-21 boards, restored from their
//! forward-migrated dumps, mint a loop shortcut at a P0 priority frame through `apply()`, and
//! the take delivers it.
//!
//! The fixtures are `scripts/migrate-dump-fixture.sh` output from `NivComboCuriosity.zip` and
//! `Magdacombo.zip` (`--effect-kind DealDamage --deck-size Exactly:100`, Magda also with
//! `--additional-phase` set to the Seize the Day value [`seize_the_day_phase`] parses);
//! regenerating either means re-running that script, which gzips its output.

use std::path::{Path, PathBuf};

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::game::engine::apply;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::committed_dump_walk::restore_committed;
use crate::fantastic_four_bounded_loop::MAX_SHORTCUT_CYCLES_MIRROR;
use crate::loop_shortcut_drain_boards::{committed_document, drive_one_beat};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Beats either board is driven before its offer must have appeared.
const BEAT_BUDGET: u32 = 80;

const SEIZE_THE_DAY: &str = "Untap target creature. After this main phase, there is an additional combat phase followed by an additional main phase.\nFlashback {2}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn niv() -> GameState {
    restore_committed(&fixture("niv_parun_curiosity_turn44_4p.json.gz")).expect("restore Niv")
}

fn magda() -> GameState {
    restore_committed(&fixture("magda_treasure_turn21_4p.json.gz")).expect("restore Magda")
}

fn name_of(state: &GameState, id: ObjectId) -> &str {
    state
        .objects
        .get(&id)
        .map_or("", |object| object.name.as_str())
}

/// Drives `state` with `beat` until a loop shortcut is offered, the game ends, or the budget
/// runs out; whether it was offered.
fn drive_to_offer(state: &mut GameState, mut beat: impl FnMut(&mut GameState)) -> bool {
    for _ in 0..BEAT_BUDGET {
        match state.waiting_for {
            WaitingFor::LoopShortcut { .. } => return true,
            WaitingFor::GameOver { .. } => return false,
            _ => beat(state),
        }
    }
    matches!(state.waiting_for, WaitingFor::LoopShortcut { .. })
}

/// Niv's drive: every damage aimed at P1, latched before the first prompt.
fn niv_offer(state: &mut GameState) -> bool {
    let mut aimed_at = Some(P1);
    drive_to_offer(state, |state| drive_one_beat(state, &mut aimed_at))
}

/// Magda's drive: at P0's empty-stack priority P0 activates Clock of Omens, tapping Battered
/// Golem and Universal Automaton to untap Universal Automaton; every other priority passes.
fn magda_beat(state: &mut GameState) {
    let who = state.waiting_for.acting_player().expect("an acting player");
    let actions = engine::ai_support::legal_actions_for_viewer(state, who).0;
    let chosen = if let WaitingFor::Priority { player } = state.waiting_for {
        let clock = (player == P0 && state.stack.is_empty())
            .then(|| {
                actions.iter().find(|action| {
                    matches!(action, GameAction::ActivateAbility { source_id, .. }
                        if name_of(state, *source_id) == "Clock of Omens")
                })
            })
            .flatten();
        clock.or_else(|| {
            actions
                .iter()
                .find(|action| matches!(action, GameAction::PassPriority))
        })
    } else {
        actions
            .iter()
            .find(|action| {
                matches!(action, GameAction::ChooseTarget { target: Some(TargetRef::Player(seat)) } if *seat == P1)
            })
            .or_else(|| {
                actions.iter().find(|action| {
                    matches!(action, GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) }
                        if name_of(state, *id) == "Universal Automaton")
                })
            })
            .or_else(|| {
                actions.iter().find(|action| match action {
                    GameAction::SelectCards { cards } => {
                        let mut names: Vec<&str> =
                            cards.iter().map(|id| name_of(state, *id)).collect();
                        names.sort_unstable();
                        names == ["Battered Golem", "Universal Automaton"]
                    }
                    _ => false,
                })
            })
            .or_else(|| {
                actions
                    .iter()
                    .find(|action| matches!(action, GameAction::DecideOptionalEffect { accept: true }))
            })
            .or_else(|| actions.first())
    }
    .cloned()
    .unwrap_or_else(|| panic!("no action at {:?}", state.waiting_for));
    let actor = engine::game::turn_control::authorized_submitter_for_player(state, who);
    apply(state, actor, chosen).expect("the beat applies");
}

/// CR 732.2a + CR 732.2b: the proposer declares `count` and every other player accepts.
fn take(state: &mut GameState, proposer: PlayerId, count: u32) {
    apply(
        state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        },
    )
    .expect("the declaration is accepted");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("the response is accepted");
    }
}

fn treasures(state: &GameState, controller: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Treasure" && object.controller == controller)
        })
        .count()
}

/// CR 732.2a + CR 104.2a: Niv-Mizzet, Parun and the Firemind each ping on every draw, so P0 is
/// offered the loop at the count that takes every opponent's life, and wins.
#[test]
fn niv_board_offers_p0_the_derived_count_and_the_take_wins() {
    let mut state = niv();
    assert!(niv_offer(&mut state), "no offer at {:?}", state.waiting_for);
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        schema,
        ..
    } = &state.waiting_for
    else {
        unreachable!()
    };
    assert_eq!(*proposer, P0);
    assert_eq!(schema.iteration_count, IterationCount::Fixed(40));
    assert_eq!(*predicted_winner, Some(P0));

    take(&mut state, P0, 40);
    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { winner: Some(P0) }),
        "{:?}",
        state.waiting_for
    );
}

/// CR 732.2a + CR 604.2: a static that reads history ("This spell costs {2} less to cast as long
/// as you've drawn two or more cards this turn.") is read whether or not anything activates it,
/// so the board with Gwaihir the Windlord in P0's hand is refused.
#[test]
fn niv_board_with_a_history_reading_static_is_refused() {
    let mut reach = niv();
    assert!(
        niv_offer(&mut reach),
        "reach guard: the unplanted board offers"
    );

    let mut state = niv();
    let db = crate::support::shared_card_db().expect("card db");
    let face = db
        .get_face_by_name("Gwaihir the Windlord")
        .expect("Gwaihir the Windlord");
    let id = engine::game::deck_loading::create_object_from_card_face(&mut state, face, P0);
    engine::game::zones::remove_from_zone(&mut state, id, Zone::Library, P0);
    engine::game::zones::add_to_zone(&mut state, id, Zone::Hand, P0);
    state.objects.get_mut(&id).expect("planted").zone = Zone::Hand;
    assert!(!niv_offer(&mut state), "offered: {:?}", state.waiting_for);
}

/// CR 732.2a: Magda's loop grows Treasures and loyalty with nothing falling, so it is offered
/// unbounded; the proposer's declared count is the number of repetitions taken, each making
/// four Treasures (two Magda triggers, each doubled by Xorn).
#[test]
fn magda_board_offers_the_unbounded_loop_and_takes_the_declared_count() {
    let mut state = magda();
    assert!(
        drive_to_offer(&mut state, magda_beat),
        "no offer at {:?}",
        state.waiting_for
    );
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        schema,
        ..
    } = &state.waiting_for
    else {
        unreachable!()
    };
    assert_eq!(*proposer, P0);
    assert_eq!(state.phase, Phase::PostCombatMain);
    assert!(state.stack.is_empty());
    assert_eq!(
        schema.iteration_count,
        IterationCount::Fixed(MAX_SHORTCUT_CYCLES_MIRROR)
    );
    assert!(!schema.is_bounded());
    assert_eq!(*predicted_winner, None);

    let before = treasures(&state, P0);
    take(&mut state, P0, 25);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(state.phase, Phase::PostCombatMain);
    assert_eq!(treasures(&state, P0), before + 100);
}

/// The `AdditionalPhase` Seize the Day parses to.
fn seize_the_day_phase() -> serde_json::Value {
    let parsed = engine::parser::oracle::parse_oracle_text(
        SEIZE_THE_DAY,
        "Seize the Day",
        &["Flashback".to_owned()],
        &["Sorcery".to_owned()],
        &[],
    );
    let mut sites = Vec::new();
    additional_phases(
        &serde_json::to_value(&parsed.abilities).expect("serialize"),
        &mut sites,
    );
    assert_eq!(sites.len(), 1, "{sites:?}");
    sites.remove(0)
}

fn additional_phases(value: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(serde_json::Value::as_str) == Some("AdditionalPhase") {
                out.push(value.clone());
            }
            map.values().for_each(|child| additional_phases(child, out));
        }
        serde_json::Value::Array(items) => {
            items.iter().for_each(|child| additional_phases(child, out))
        }
        _ => {}
    }
}

/// CR 500.8: the migrated Magda dump's Seize the Day carries the extra phases the card's text
/// parses to, "After this main phase, there is an additional combat phase followed by an
/// additional main phase."
#[test]
fn magda_fixture_extra_phases_are_the_cards() {
    let mut sites = Vec::new();
    additional_phases(
        &committed_document(&fixture("magda_treasure_turn21_4p.json.gz")),
        &mut sites,
    );
    assert!(
        !sites.is_empty(),
        "reach guard: the dump carries Seize the Day"
    );
    let card = seize_the_day_phase();
    assert!(sites.iter().all(|site| *site == card), "{sites:?}");
}

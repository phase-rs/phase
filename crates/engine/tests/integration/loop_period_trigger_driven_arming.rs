//! CR 603.3 + CR 608.2 + CR 111.1: a token-minting triggered ability's resolution is traced for
//! its controller, and the CR 732.2a loop period it repeats is offered.
//!
//! Two boards, because the two cycles carry their choice and their mint at different beats.
//! Board A (built by `abdel_adrian_animate_dead_altar_board`, driven here and not rebuilt) makes
//! the choice and the mint inside ONE triggered ability's resolution. Board B, built below, makes
//! the choice on the copy's enters trigger and the mint on Preston's.
//!
//! **Board B's provenance.** Every card comes from the committed integration card fixture.
//!
//! | Card | Seat / zone | Why it is here |
//! |---|---|---|
//! | Preston, the Vanisher | P0 battlefield | the minting trigger: "whenever another nontoken creature you control enters, if it wasn't cast, create a token that's a copy of that creature, except it's a 0/1 white Illusion" |
//! | Altar of the Brood | P0 battlefield | a triggered ability on the same board whose resolution the arming places OUTSIDE the class |
//! | Felidar Guardian | P0 graveyard | what Animate Dead returns uncast, and what the copy's enters trigger blinks |
//! | Animate Dead | P0 hand | returns Felidar Guardian uncast, which is what makes Preston's "if it wasn't cast" condition hold |
//! | two basic Swamps | P0 battlefield | P0's lands |
//! | seeded `{B}{B}` | P0's mana pool | what pays Animate Dead's `{1}{B}`; the drive taps nothing and activates no mana ability |
//! | basic Swamps, `LIBRARY_PER_SEAT` each | every seat's library | sized so no seat decks out under Altar's mill and the decline policy below |
//!
//! **Board B's decline policy.** Felidar Guardian's exile is voluntary ("you may exile"). The
//! drive ACCEPTS that choice, aimed at the nontoken Felidar Guardian, while accepts remain, and
//! DECLINES at the first choice of that kind after them. Board A's row answers the enters
//! trigger's exile choice with both other nonland permanents once, and answers every later choice
//! of that kind with an empty selection.

use engine::analysis::loop_check::OfferRoad;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use engine::game::scenario_db::GameScenarioDbExt;

use crate::support::shared_card_db as load_db;

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const SEATS: [PlayerId; 4] = [P0, P1, P2, P3];

/// Basic lands per seat. Read by no assertion; it exists so Altar's mill and the settle below
/// cannot end on a draw from an empty library.
const LIBRARY_PER_SEAT: usize = 40;

/// How many of Preston's mints Board B's row drives before it declines.
const CYCLES_DRIVEN: usize = 3;

/// Beat cap for every drive loop here. Read by no assertion; it bounds a runaway drive.
const BEAT_CAP: usize = 240;

pub(super) struct PrestonBoard {
    pub(super) runner: GameRunner,
    pub(super) felidar: ObjectId,
    pub(super) animate_dead: ObjectId,
    pub(super) preston: ObjectId,
    pub(super) altar: ObjectId,
}

pub(super) fn build_board_b() -> Option<PrestonBoard> {
    let db = load_db()?;
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let preston = scenario.add_real_card(P0, "Preston, the Vanisher", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Altar of the Brood", Zone::Battlefield, db);
    let felidar = scenario.add_real_card(P0, "Felidar Guardian", Zone::Graveyard, db);
    let animate_dead = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    }
    for seat in SEATS {
        for _ in 0..LIBRARY_PER_SEAT {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    Some(PrestonBoard {
        runner,
        felidar,
        animate_dead,
        preston,
        altar,
    })
}

/// The window's trace entries.
fn entries(state: &GameState) -> Vec<engine::game::TraceEntry> {
    engine::game::play_trace_view(state).map_or_else(Vec::new, |view| view.entries)
}

/// The window's choices: its plays and its optional answers (CR 732.3).
fn choices(state: &GameState) -> Vec<engine::game::TraceEntry> {
    use engine::game::{AnswerOptionality, EntryKind};
    entries(state)
        .into_iter()
        .filter(|entry| match &entry.kind {
            EntryKind::Play { .. } => true,
            EntryKind::Answer { optional, .. } => *optional == AnswerOptionality::Optional,
            EntryKind::Resolution { .. } => false,
        })
        .collect()
}

/// The seat of the newest triggered-ability resolution in `entries`.
fn last_resolution_seat(entries: &[engine::game::TraceEntry]) -> Option<PlayerId> {
    entries
        .iter()
        .rev()
        .find(|entry| matches!(entry.kind, engine::game::EntryKind::Resolution { .. }))
        .map(|entry| entry.seat)
}

fn token_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|object| object.is_token))
        .count()
}

/// The stack's top entry when it is a triggered ability, as `(entry id, source id)`.
fn top_trigger(state: &GameState) -> Option<(ObjectId, ObjectId)> {
    let entry = state.stack.back()?;
    match &entry.kind {
        StackEntryKind::TriggeredAbility { source_id, .. } => Some((entry.id, *source_id)),
        _ => None,
    }
}

fn cast_animate_dead(runner: &mut GameRunner, animate_dead: ObjectId, onto: ObjectId) {
    let card_id = runner
        .state()
        .objects
        .get(&animate_dead)
        .expect("Animate Dead")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: animate_dead,
            card_id,
            targets: vec![onto],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Animate Dead is castable with the seeded mana");
}

/// What one drive observed, so a row asserts over it rather than over a running commentary.
#[derive(Default)]
struct DriveReading {
    /// The trace read at each beat where a token had just appeared.
    at_mint: Vec<Vec<engine::game::TraceEntry>>,
    /// `(choices before, choices after)` taken across each resolution of a triggered ability that
    /// mints no token.
    across_out_of_class: Vec<(Vec<engine::game::TraceEntry>, Vec<engine::game::TraceEntry>)>,
    /// The first loop-shortcut offer the drive met, as its road and the stack's top triggered
    /// ability's source at that beat. The drive declines every offer and continues.
    offered: Option<(OfferRoad, Option<ObjectId>)>,
}

/// One beat of a drive at a time, answering by the row's own declared policy.
///
/// `exile_target` names the permanent the voluntary exile is aimed at while `accepts_left` is
/// positive; `zone_choice_accepts` answers a zone choice in full that many times and with an empty
/// selection after.
fn drive(
    runner: &mut GameRunner,
    out_of_class: &[ObjectId],
    exile_target: Option<ObjectId>,
    mut accepts_left: usize,
    mut zone_choice_accepts: usize,
) -> DriveReading {
    let mut reading = DriveReading::default();
    let mut tokens = token_count(runner.state());
    // The out-of-class entry whose resolution is being watched, with the window's choices as they
    // stood before that resolution began.
    let mut watching: Option<(ObjectId, Vec<engine::game::TraceEntry>)> = None;

    for _ in 0..BEAT_CAP {
        let state = runner.state();
        if let WaitingFor::LoopShortcut { road, .. } = state.waiting_for {
            reading
                .offered
                .get_or_insert((road, top_trigger(state).map(|(_, source)| source)));
            // Declining clears the trace, so a resolution watched across it is not compared.
            watching = None;
            if runner.act(GameAction::DeclineShortcut).is_err() {
                break;
            }
            continue;
        }
        let seq = choices(state);

        // A token has appeared since the previous beat: read the trace at that beat.
        let now = token_count(state);
        if now > tokens {
            reading.at_mint.push(entries(state));
        }
        tokens = now;

        // The watched out-of-class entry has left the stack, so its resolution is behind us.
        if let Some((entry_id, before)) = watching.clone() {
            if !state.stack.iter().any(|entry| entry.id == entry_id) {
                reading.across_out_of_class.push((before, seq.clone()));
                watching = None;
            }
        }
        if watching.is_none() {
            if let Some((entry_id, source_id)) = top_trigger(state) {
                if out_of_class.contains(&source_id) {
                    watching = Some((entry_id, seq.clone()));
                }
            }
        }

        let legal = engine::ai_support::legal_actions(state);
        let aimed = exile_target
            .filter(|_| accepts_left > 0)
            .and_then(|target| {
                legal
                    .iter()
                    .find(|action| {
                        matches!(
                            action,
                            GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) }
                                if *id == target
                        )
                    })
                    .cloned()
            });
        let action = match aimed {
            Some(action) => action,
            None => {
                let accept = accepts_left > 0;
                let optional = legal
                    .iter()
                    .find(|action| {
                        matches!(
                            action,
                            GameAction::DecideOptionalEffect { accept: answered }
                                if *answered == accept
                        )
                    })
                    .cloned();
                match optional {
                    Some(action) => {
                        if accept {
                            accepts_left -= 1;
                        }
                        action
                    }
                    // A zone choice is not enumerated in the legal-action list (its answer space
                    // is the offered set's power set), so it is answered off the waiting state.
                    None => match &state.waiting_for {
                        WaitingFor::EffectZoneChoice { cards, .. } => {
                            let answer = if zone_choice_accepts > 0 {
                                zone_choice_accepts -= 1;
                                cards.clone()
                            } else {
                                vec![]
                            };
                            GameAction::SelectCards { cards: answer }
                        }
                        _ => legal
                            .iter()
                            .find(|action| !matches!(action, GameAction::PassPriority))
                            .cloned()
                            .unwrap_or(GameAction::PassPriority),
                    },
                }
            }
        };
        if runner.act(action).is_err() {
            break;
        }
    }
    reading
}

/// CR 603.3 + CR 608.2 + CR 111.1 + CR 732.2a: Board A — the cycle's choice and its mint sit in ONE
/// triggered ability's resolution (Abdel Adrian's enters trigger exiles any number of other nonland
/// permanents, then mints a Soldier for each permanent exiled this way). That resolution is traced
/// for its controller, and the period it repeats is offered.
#[test]
fn board_a_minting_trigger_resolution_opens_a_period_naming_that_trigger() {
    let Some(mut board) = crate::abdel_adrian_animate_dead_altar_board::build() else {
        return;
    };
    board.runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    assert!(
        entries(board.runner.state()).is_empty(),
        "nothing is traced before anything is driven"
    );

    cast_animate_dead(&mut board.runner, board.animate_dead, board.abdel);
    let reading = drive(
        &mut board.runner,
        &[board.altar, board.animate_dead],
        None,
        0,
        1,
    );

    // CR 117.3b/c: the span a repeat names is offered at the first window after the repeated
    // resolution, before Abdel Adrian's trigger is back on top.
    assert!(
        matches!(
            reading.offered,
            Some((OfferRoad::RecordedPeriod, top)) if top != Some(board.abdel)
        ),
        "the period is offered on the recorded road before Abdel Adrian's trigger stands on top \
         again; got {:?}",
        reading.offered
    );
    let at_mint = reading
        .at_mint
        .first()
        .expect("a Soldier token appeared, so the minting trigger resolved");
    assert_eq!(
        last_resolution_seat(at_mint),
        Some(P0),
        "the minting resolution is traced for the trigger's controller"
    );

    let (before, after) = reading
        .across_out_of_class
        .iter()
        .find(|(before, _)| !before.is_empty())
        .expect(
            "Altar of the Brood's mill trigger or Animate Dead's own trigger resolved at a beat \
             where a period already stood",
        );
    assert_eq!(
        before, after,
        "a triggered ability that mints nothing adds no choice to the window — asserted across \
         that member's own resolution, at a beat where the window holds choices that member did \
         not cause"
    );
}

/// CR 603.3 + CR 608.2 + CR 111.1 + CR 704.5m: Board B — the cycle's choice sits on the copy's
/// enters trigger and its mint on Preston's. Each of Preston's resolutions is traced for its
/// controller.
#[test]
fn board_b_minting_trigger_opens_a_period_with_choice_and_mint_on_different_triggers() {
    let Some(mut board) = build_board_b() else {
        return;
    };
    assert!(
        entries(board.runner.state()).is_empty(),
        "nothing is traced before anything is driven"
    );

    cast_animate_dead(&mut board.runner, board.animate_dead, board.felidar);
    let reading = drive(
        &mut board.runner,
        &[board.altar, board.animate_dead],
        Some(board.felidar),
        CYCLES_DRIVEN + 1,
        0,
    );

    assert!(
        reading.at_mint.len() >= CYCLES_DRIVEN,
        "the drive observed at least {CYCLES_DRIVEN} of Preston's mints, so the cycle repeated \
         rather than firing once"
    );
    for at_mint in &reading.at_mint {
        assert_eq!(
            last_resolution_seat(at_mint),
            Some(P0),
            "each minting resolution is traced for the trigger's controller"
        );
    }

    let (before, after) = reading
        .across_out_of_class
        .iter()
        .find(|(before, _)| !before.is_empty())
        .expect(
            "Altar of the Brood's mill trigger or Animate Dead's own trigger resolved at a beat \
             where a period already stood",
        );
    assert_eq!(
        before, after,
        "a triggered ability that mints nothing adds no choice to the window"
    );

    // CR 704.5m: an Aura that is not attached to an object is put into its owner's graveyard.
    // Blinking the creature Animate Dead enchants is what detaches it.
    assert_eq!(
        board
            .runner
            .state()
            .objects
            .get(&board.animate_dead)
            .map(|object| object.zone),
        Some(Zone::Graveyard),
        "Animate Dead is in its owner's graveyard once the creature it enchanted was blinked"
    );
    assert_eq!(
        board
            .runner
            .state()
            .objects
            .get(&board.felidar)
            .map(|object| object.zone),
        Some(Zone::Battlefield),
        "Felidar Guardian returned to the battlefield, which is what re-fired Preston's trigger"
    );
}

/// CR 605.3a: the live control the two rows' absence-shaped legs rest on — a choice a player DOES
/// make, moving the same trace through the same `apply()` boundary. Board B's legal-action list offers no land tap, so the control is BUILT from the
/// engine's own option surface rather than picked out of that list.
#[test]
fn a_built_land_tap_moves_the_same_record_through_the_same_boundary() {
    let Some(mut board) = build_board_b() else {
        return;
    };
    let selection = {
        let state = board.runner.state();
        state
            .battlefield
            .iter()
            .filter_map(|id| {
                if state.objects.get(id)?.controller != P0 {
                    return None;
                }
                let option =
                    engine::game::mana_sources::activatable_land_mana_options(state, *id, P0)
                        .into_iter()
                        .next()?;
                option.semantic_selection(state)
            })
            .next()
            .expect("the engine's own option surface offers a land-mana option on this board")
    };
    // Two legs that must disagree: the option surface above offers a land-mana option while the
    // legal-action list offers no land tap. Agreement would mean the control was picked rather
    // than built, and a picked control proves nothing about this board.
    assert!(
        !engine::ai_support::legal_actions(board.runner.state())
            .iter()
            .any(|action| matches!(action, GameAction::TapLandForMana { .. })),
        "the legal-action list offers no land tap on this board, which is why the control is built"
    );

    assert!(choices(board.runner.state()).is_empty());
    board
        .runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the engine's own semantic selection is accepted");
    let seq = choices(board.runner.state());
    assert_eq!(
        seq.len(),
        1,
        "the land tap is one choice, so an unchanged reading across a resolution is a result and \
         not a dead instrument"
    );
    assert!(
        matches!(
            seq[0].kind,
            engine::game::EntryKind::Play {
                action: GameAction::TapLandForMana { .. },
                ..
            }
        ),
        "the choice the control traces is the land-mana one"
    );
}

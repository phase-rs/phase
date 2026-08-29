//! CR 732.2a acceptance on BOTH gate boards — a Sprout Swarm object-growth loop whose period
//! ALSO mills every opponent must offer the shortcut, and the offer must NAME the mill.
//!
//! Loop shape (Oracle text verbatim from the pinned export): Sprout Swarm (Convoke + Buyback
//! {3}) in P0's hand, Witherbloom the Balancer granting affinity for creatures, Altar of the
//! Brood milling each opponent whenever another permanent P0 controls enters. Affinity zeroes
//! the generic and convoke pays the {G} off an untapped Saproling (CR 702.51a / CR 702.27 /
//! CR 702.41), so a cycle is free: +1 or +2 Saprolings, each opponent mills, NO life change,
//! P0's own library untouched. Per CR 701.17b + CR 121.4 an empty library neither stops the
//! mill nor ends the game, so the opponents' library sizes must not bound the loop.
//!
//! THE MODULE SPLITS ON ONE AXIS. Every arm asserts an offer whose `unbounded` carries
//! `TokensCreated` — both kinds of board produce the token growth, so that is the shared
//! conjunct and marks neither side. The split is `ResourceAxis::LibraryDelta`: every
//! Altar-BEARING arm asserts one per victim, every Altar-ABSENT arm asserts none. The victim
//! set is derived from the live cast's own library decline rather than pinned, so the two
//! halves are read by the same instrument.
//!
//! Neither board substitutes for the other. `witherbloom_altar_sprout_swarm_4p` is the owner's
//! dumped board — a viewer projection whose library and hand objects are already typeless
//! placeholders, so redaction cannot move it; it is the ship gate. `sprout_witherbloom_
//! realistic_lands_4p` plus a grafted Altar is an authoritative capture whose libraries hold
//! real cards, so it is the only dump that exercises the proposer-view redaction end to end.

use engine::analysis::loop_check::WinKind;
use engine::analysis::resource::ResourceAxis;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::sprout_inalla_realistic_offer::{drive_sprout_cast, load_realistic_dump};

const P0: PlayerId = PlayerId(0);
const SPROUT: ObjectId = ObjectId(55);
const ALTAR: ObjectId = ObjectId(90);
const DOUBLING_SEASON: ObjectId = ObjectId(67);
/// An untapped P0 green Saproling to convoke for the {G} (417, 422, 432, 436, 437 are untapped).
const FODDER: ObjectId = ObjectId(417);
/// P3's Pyreswipe Hawk — its `Attacks` body is a `Pump` off a board aggregate, a ledger read
/// the growing-class firewall vetoes on when it is scanned.
const PYRESWIPE_HAWK: ObjectId = ObjectId(298);
const PIT_OF_OFFERINGS: ObjectId = ObjectId(9);

/// Altar of the Brood, VERBATIM Oracle text from the pinned `client/public/card-data.json`
/// export — the graft the authoritative capture needs to carry this loop's mill.
pub(super) const ALTAR_ORACLE: &str =
    "Whenever another permanent you control enters, each opponent mills a card.";

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load through the REAL production restore chokepoint, exactly as the sibling Sprout tests do.
fn load_wb() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/witherbloom_altar_sprout_swarm_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
}

/// Put a real parsed Altar of the Brood on `seat`'s battlefield.
///
/// `create_object` plus `push_printed_trigger`, never `objects.insert` +
/// `battlefield.push_back`: the latter leaves the definition out of `base_trigger_definitions`,
/// `game/layers.rs`'s per-pass reset then drops the live entry, and the board behaves as though
/// the permanent were absent — a green arm for the wrong reason. Same law
/// `wba_fodder_multiset::graft_doubler` records on the replacement side.
pub(super) fn graft_altar(state: &mut GameState, seat: PlayerId) -> ObjectId {
    let parsed = engine::parser::parse_oracle_text(
        ALTAR_ORACLE,
        "Altar of the Brood",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers.len(),
        1,
        "fixture pin: Altar of the Brood parses to exactly ONE trigger definition — the \
         CR 603.6a entry observer whose body is the mill every arm here is about"
    );
    assert!(
        parsed.abilities.is_empty() && parsed.statics.is_empty() && parsed.replacements.is_empty(),
        "fixture pin: Altar of the Brood carries no activated, static or replacement surface, \
         so the grafted object's only new speaker is that one trigger"
    );
    let card_id = CardId(state.next_object_id);
    let host = create_object(
        state,
        card_id,
        seat,
        "Altar of the Brood".to_string(),
        Zone::Battlefield,
    );
    let obj = state
        .objects
        .get_mut(&host)
        .expect("the just-created Altar is in `objects`");
    obj.card_types.core_types = vec![CoreType::Artifact];
    obj.push_printed_trigger(parsed.triggers[0].clone());
    host
}

fn count_saprolings(state: &GameState, who: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == who && o.name == "Saproling")
        })
        .count()
}

fn library_sizes(state: &GameState) -> Vec<(PlayerId, usize)> {
    state
        .players
        .iter()
        .map(|p| (p.id, p.library.len()))
        .collect()
}

/// The players whose libraries actually declined across the driven cast — the loop's own
/// victims, read off the board instead of pinned, so the two halves of the split below are
/// measured by ONE instrument.
fn victims(before: &GameState, after: &GameState) -> Vec<PlayerId> {
    let after_sizes = library_sizes(after);
    library_sizes(before)
        .into_iter()
        .zip(after_sizes)
        .filter(|((_, was), (_, now))| now < was)
        .map(|((id, _), _)| id)
        .collect()
}

/// The players named by a `LibraryDelta` axis in `unbounded`.
fn library_delta_players(unbounded: &[ResourceAxis]) -> Vec<PlayerId> {
    unbounded
        .iter()
        .filter_map(|axis| match axis {
            ResourceAxis::LibraryDelta(player) => Some(*player),
            _ => None,
        })
        .collect()
}

fn remove(state: &mut GameState, id: ObjectId) {
    state.battlefield.retain(|x| *x != id);
    state.objects.remove(&id);
}

/// One live Sprout Swarm cycle on the dumped board: accept Buyback, convoke `fodder` for the
/// {G}, commit, resolve.
fn cast_wb(state: GameState, fodder: ObjectId) -> GameState {
    GameRunner::from_state(state)
        .cast(SPROUT)
        .accept_optional()
        .convoke_with(&[fodder])
        .commit()
        .resolve()
        .state()
        .clone()
}

// ── Board suppliers: each is one arm's (loader, mutations) half ──

fn wb_dumped() -> GameState {
    load_wb()
}
fn wb_no_altar() -> GameState {
    let mut state = load_wb();
    remove(&mut state, ALTAR);
    state
}
fn wb_no_doubling_season() -> GameState {
    let mut state = load_wb();
    remove(&mut state, DOUBLING_SEASON);
    state
}
fn wb_no_doubling_season_no_altar() -> GameState {
    let mut state = wb_no_doubling_season();
    remove(&mut state, ALTAR);
    state
}
fn wb_mill_isolated() -> GameState {
    let mut state = wb_no_doubling_season();
    remove(&mut state, PYRESWIPE_HAWK);
    state
}
fn wb_mill_isolated_no_altar() -> GameState {
    let mut state = wb_mill_isolated();
    remove(&mut state, ALTAR);
    state
}
fn wb_peeled_no_altar() -> GameState {
    let mut state = load_wb();
    for id in [DOUBLING_SEASON, ALTAR, PYRESWIPE_HAWK, PIT_OF_OFFERINGS] {
        remove(&mut state, id);
    }
    state
}
fn capture_with_altar() -> GameState {
    let mut state = load_realistic_dump();
    graft_altar(&mut state, P0);
    state
}

// ── Drivers: one per board, since each dump has its own Sprout and fodder ids ──

fn drive_wb_one_cycle(state: GameState) -> GameState {
    cast_wb(state, FODDER)
}

/// Drive Sprout Swarm cycles until the offer is up or the untapped fodder runs out, so an arm
/// whose board needed more history than one cycle still reports the offer it eventually forms
/// rather than reading as silence.
fn drive_wb_until_offer(mut state: GameState) -> GameState {
    for _ in 0..5 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            return state;
        }
        let pick = state
            .battlefield
            .iter()
            .find(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.controller == P0 && o.name == "Saproling" && !o.tapped)
            })
            .copied()
            .expect("multi-cycle arm: an untapped P0 Saproling to convoke");
        state = cast_wb(state, pick);
    }
    state
}

fn drive_capture_one_cycle(state: GameState) -> GameState {
    drive_sprout_cast(state).state().clone()
}

/// What an arm asserts about `ResourceAxis::LibraryDelta` — the one axis this module splits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MillAxis {
    /// Altar-BEARING: one axis per victim, and no axis for a non-victim.
    OnePerVictim,
    /// Altar-ABSENT: none at all. Asserting the ABSENT axis is what stops the module passing
    /// on a change that made every board publish a mill.
    None,
}

/// One arm: a board supplier (loader + mutations), the live cast that drives it, and the
/// expected mill axis.
struct GateArm {
    what: &'static str,
    board: fn() -> GameState,
    drive: fn(GameState) -> GameState,
    expect: MillAxis,
}

/// Drive one arm and report every way it disagrees with its expectation, rather than stopping
/// at the first — the split below is only evidence when BOTH halves have been read.
fn run(arm: &GateArm) -> Vec<String> {
    let what = arm.what;
    let before = (arm.board)();
    let saprolings_before = count_saprolings(&before, P0);
    let before_snapshot = before.clone();
    let after = (arm.drive)(before);

    // Reach-guards that hold on BOTH sides of the split, so a reported axis disagreement is
    // never "the harness drove nothing".
    assert!(
        count_saprolings(&after, P0) > saprolings_before,
        "{what} reach-guard: the driven cycle must create at least one Saproling"
    );
    assert!(
        after
            .objects
            .values()
            .any(|o| o.name == "Sprout Swarm" && o.controller == P0 && o.zone == Zone::Hand),
        "{what} reach-guard: CR 702.27a buyback must return Sprout Swarm to P0's hand, i.e. \
         the cast really resolved and the loop is recastable"
    );

    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
        ..
    } = &after.waiting_for
    else {
        return vec![format!(
            "{what}: the CR 732.2a object-growth shortcut must be OFFERED, got {:?}",
            after.waiting_for
        )];
    };

    let mut wrong = Vec::new();
    if *proposer != P0 {
        wrong.push(format!(
            "{what}: the loop's controller must be the proposer, got {proposer:?}"
        ));
    }
    if certificate.win_kind != WinKind::Advantage {
        wrong.push(format!(
            "{what}: an unbounded mill an empty library neither stops (CR 701.17b) nor ends \
             the game on (CR 121.4) is an ADVANTAGE loop, not a win the collapse can deliver; \
             got {:?}",
            certificate.win_kind
        ));
    }
    if !certificate.unbounded.contains(&ResourceAxis::TokensCreated) {
        wrong.push(format!(
            "{what}: the shared conjunct — both kinds of board grow the Saproling class, got \
             {:?}",
            certificate.unbounded
        ));
    }

    let milled = victims(&before_snapshot, &after);
    let published = library_delta_players(&certificate.unbounded);
    match arm.expect {
        MillAxis::OnePerVictim => {
            assert!(
                !milled.is_empty(),
                "{what} paired reach-guard: an Altar-bearing board's own cast must decline at \
                 least one opponent library, or the axis reading below is vacuous"
            );
            if published != milled {
                wrong.push(format!(
                    "{what}: CR 701.17a — the certificate must publish one LibraryDelta per \
                     victim {milled:?} and none for a non-victim, got {published:?}"
                ));
            }
        }
        MillAxis::None => {
            assert!(
                milled.is_empty(),
                "{what} paired reach-guard: an Altar-absent board's cast must mill nobody, so \
                 the empty axis reading below is about the certificate and not the board"
            );
            if !published.is_empty() {
                wrong.push(format!(
                    "{what}: with no mill in the period the certificate must publish NO \
                     LibraryDelta axis, got {published:?}"
                ));
            }
        }
    }
    wrong
}

/// Walk every arm before asserting, so one disagreeing arm does not hide the rest.
fn run_all(arms: &[GateArm]) {
    let wrong: Vec<String> = arms.iter().flat_map(run).collect();
    assert!(
        wrong.is_empty(),
        "{} of {} arms disagreed:\n  {}",
        wrong.len(),
        arms.len(),
        wrong.join("\n  ")
    );
}

/// The Altar-BEARING half of the split. Arm 0 is the ship gate; the rest vary cycle count,
/// token doubling and the two firewall-adjacent permanents around it, so the published mill
/// axis is a property of the Altar and not of one board configuration.
const BEARING: [GateArm; 5] = [
    GateArm {
        what: "SHIP GATE — the owner's dumped board, unmutated",
        board: wb_dumped,
        drive: drive_wb_one_cycle,
        expect: MillAxis::OnePerVictim,
    },
    GateArm {
        what: "the authoritative capture plus a grafted Altar",
        board: capture_with_altar,
        drive: drive_capture_one_cycle,
        expect: MillAxis::OnePerVictim,
    },
    GateArm {
        what: "the dumped board driven until the offer forms",
        board: wb_dumped,
        drive: drive_wb_until_offer,
        expect: MillAxis::OnePerVictim,
    },
    GateArm {
        what: "the dumped board with Doubling Season removed (one token per cycle)",
        board: wb_no_doubling_season,
        drive: drive_wb_one_cycle,
        expect: MillAxis::OnePerVictim,
    },
    GateArm {
        what: "the dumped board with Doubling Season and Pyreswipe Hawk removed",
        board: wb_mill_isolated,
        drive: drive_wb_one_cycle,
        expect: MillAxis::OnePerVictim,
    },
];

/// The Altar-ABSENT half: the same boards with the mill's only source removed. Matched one for
/// one against [`BEARING`] except for the peel arm, which removes three further permanents to
/// show the empty axis is not one board's accident.
const ABSENT: [GateArm; 6] = [
    GateArm {
        what: "the owner's dumped board with Altar removed",
        board: wb_no_altar,
        drive: drive_wb_one_cycle,
        expect: MillAxis::None,
    },
    GateArm {
        what: "the authoritative capture, ungrafted",
        board: load_realistic_dump,
        drive: drive_capture_one_cycle,
        expect: MillAxis::None,
    },
    GateArm {
        what: "the dumped board with Altar removed, driven until the offer forms",
        board: wb_no_altar,
        drive: drive_wb_until_offer,
        expect: MillAxis::None,
    },
    GateArm {
        what: "the dumped board with Doubling Season and Altar removed",
        board: wb_no_doubling_season_no_altar,
        drive: drive_wb_one_cycle,
        expect: MillAxis::None,
    },
    GateArm {
        what: "the dumped board with Doubling Season, Pyreswipe Hawk and Altar removed",
        board: wb_mill_isolated_no_altar,
        drive: drive_wb_one_cycle,
        expect: MillAxis::None,
    },
    GateArm {
        what: "the dumped board peeled to Doubling Season, Altar, Pyreswipe Hawk and Pit of \
               Offerings removed",
        board: wb_peeled_no_altar,
        drive: drive_wb_one_cycle,
        expect: MillAxis::None,
    },
];

/// Preconditions on the dumped board, asserted once so the arms below need not restate them.
#[test]
fn dumped_gate_board_is_the_configuration_the_arms_assume() {
    let state = load_wb();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "fixture precondition: ordinary priority for P0, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state
            .objects
            .get(&SPROUT)
            .map(|o| (o.name.as_str(), o.zone)),
        Some(("Sprout Swarm", Zone::Hand)),
        "fixture precondition: Sprout Swarm is in P0's hand"
    );
    assert_eq!(
        state.objects.get(&ALTAR).map(|o| o.name.as_str()),
        Some("Altar of the Brood"),
        "fixture precondition: the mill's source is on the board"
    );
    let fodder = state.objects.get(&FODDER).expect("fodder present");
    assert!(
        fodder.name == "Saproling" && fodder.controller == P0 && !fodder.tapped,
        "fixture precondition: {FODDER:?} is an untapped P0 fodder Saproling"
    );
}

/// **THE SHIP GATE, and its variations.** Every Altar-bearing board offers the CR 732.2a
/// shortcut and the certificate names one `LibraryDelta` per victim beside `TokensCreated`.
///
/// DISCRIMINATING, MEASURED over these arms: restore `class_members` to the growth set
/// unrestricted by the scanned frame's battlefield residency
/// (`analysis::resource::loop_states_cover_modulo_fodder_growth`) ⇒ the firewall vetoes on the
/// certified graveyard-resident ids and all four DUMPED-board arms redden at `Priority{P0}`,
/// while [`altar_absent_boards_publish_no_library_delta`] stays green. The authoritative
/// capture's arm is invariant under it — that board carries no consulting site the dropped ids
/// move — which is why the dumped board and not the capture is what pins the restriction.
#[test]
fn altar_bearing_boards_offer_and_publish_one_library_delta_per_victim() {
    run_all(&BEARING);
}

/// The paired positive for the row above, on the SAME instrument: with the mill's only source
/// gone the same boards still offer, and the certificate publishes NO `LibraryDelta`.
///
/// DISCRIMINATING: drop `certify_instructed_opponent_library_departure`'s C1c empty-set guard
/// so a period with no departure still yields a certificate ⇒ these arms publish a mill axis
/// for a board that mills nobody ⇒ **FAILS**.
#[test]
fn altar_absent_boards_publish_no_library_delta() {
    run_all(&ABSENT);
}

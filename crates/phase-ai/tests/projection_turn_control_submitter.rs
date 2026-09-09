//! AI projection dispatched the decision *seat* as the authenticated actor, so
//! every projection under a CR 723 player-control effect bailed with
//! `EngineRejected(WrongPlayer)`.
//!
//! CR 723.5: while controlling another player, the controller makes all that
//! player's choices — so the seat holding a decision and the player authorized
//! to submit it are different players. `project_to` forwarded its one `actor`
//! into both the authenticated-actor and semantic-owner slots of the action
//! boundary, and `check_actor_authorization` refused the seat. CR 723.3 keeps
//! the seat's objects its own, so the seat must stay the semantic owner rather
//! than be remapped. CR 723.9 (self-control) must remain a no-op.

use engine::game::engine::apply_for_simulation;
use engine::game::public_state::sync_waiting_for;
use engine::game::scenario::GameScenario;
use engine::game::turn_control::authorized_submitter_for_player;
use engine::game::EngineError;
use engine::types::ability::{CastingPermission, ManaSpendPermission};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::util::Deadline;
use phase_ai::projection::{project_to, ProjectionHorizon};
use phase_ai::saved_state::load_saved_game_state;
use std::io::Read;

/// The 2p capture's active player, and the seat put under control below.
const SEAT: PlayerId = PlayerId(1);
/// The other seat in the 2p capture, used as the turn controller.
const CONTROLLER: PlayerId = PlayerId(0);

fn gunzip_fixture(gz: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Turn-4 two-player capture, already committed and loaded by
/// `manapayment_finalize_from_floating_pool.rs`.
fn load_2p_capture() -> GameState {
    let raw = gunzip_fixture(include_bytes!(
        "../fixtures/scenarios/galvanic-blast-manapayment-turn4.json.gz"
    ));
    load_saved_game_state(&raw).expect("the turn-4 capture deserializes and restores")
}

/// Turn-25 four-player capture, already committed and loaded by `scenarios.rs`.
fn load_4p_capture() -> GameState {
    let raw = gunzip_fixture(include_bytes!(
        "../fixtures/scenarios/invisible-woman-cosmic-crucible-mana.json.gz"
    ));
    load_saved_game_state(&raw).expect("the turn-25 capture deserializes and restores")
}

/// Install turn control the way the engine's CR 723 regression tests do.
///
/// The `priority_player`/`turn_decision_controller` pairing is load-bearing,
/// not cosmetic: the reducer's `lands_tapped_for_mana` insert keys on
/// `state.priority_player`, so a stale one books a traversal's mana taps to a
/// different player than live play would. `sync_waiting_for` is the only public
/// re-synchronizer — it passes the dump's own window straight back in and
/// re-derives `priority_player` through `sync_priority_player_from_waiting_for`.
fn with_turn_control(base: &GameState, controller: PlayerId) -> GameState {
    let mut state = base.clone();
    state.turn_decision_controller = Some(controller);
    let waiting_for = state.waiting_for.clone();
    sync_waiting_for(&mut state, &waiting_for);
    state
}

/// Non-vacuity guard. `effective_authority_for_player` redirects a seat only
/// when it is the active player (individual-seat topology) — a capture whose
/// active player drifted, or a controller equal to the seat, would silently
/// take the uncontrolled path and pass for free.
fn assert_control_is_discriminating(state: &GameState, seat: PlayerId, controller: PlayerId) {
    assert_ne!(
        controller, seat,
        "a controller equal to the seat is CR 723.9 self-control, not the redirecting case"
    );
    assert_eq!(
        state.active_player, seat,
        "the capture's active player must be the seat under control, or the \
         redirect never fires"
    );
    assert!(
        !state.format_config.topology().has_shared_team_turns(),
        "these captures are individual-seat topology; under shared team turns \
         the redirect gate is team membership and this guard would be the wrong one"
    );
}

/// The horizon assertions shared by the controlled and uncontrolled legs.
fn assert_projected_past_the_turn_boundary(base: &GameState, controlled: &GameState, label: &str) {
    let projection = project_to(
        controlled,
        CONTROLLER,
        SEAT,
        ProjectionHorizon::OpponentBeginCombat,
        Deadline::none(),
    )
    .unwrap_or_else(|bail| panic!("{label}: projection bailed with {bail:?}"));

    // The discriminating assertion: the capture starts in the seat's own
    // EndCombat, so reaching its next BeginCombat requires crossing a turn
    // boundary, which requires real dispatches the boundary accepted.
    assert!(
        projection.state.turn_number > base.turn_number,
        "{label}: projection did not advance past turn {} (ended at {})",
        base.turn_number,
        projection.state.turn_number
    );
    // Shape pins, entailed by the projection returning Ok at all — they restate
    // conjuncts of `reached_horizon`, and discriminate nothing on their own.
    assert_eq!(
        projection.state.active_player, SEAT,
        "{label}: horizon seat"
    );
    assert_eq!(
        projection.state.phase,
        Phase::BeginCombat,
        "{label}: horizon phase"
    );
}

/// Row 1. Under turn control the projection must complete and advance. Reverting
/// the `project_to` dispatch to `apply_for_simulation(&mut state, seat, action)`
/// reds this at `EngineRejected(WrongPlayer)`.
#[test]
fn projection_advances_under_turn_control() {
    let base = load_2p_capture();
    let controlled = with_turn_control(&base, CONTROLLER);
    assert_control_is_discriminating(&controlled, SEAT, CONTROLLER);
    assert_eq!(
        authorized_submitter_for_player(&controlled, SEAT),
        CONTROLLER,
        "the seat's decisions are submittable only by the controller, or there \
         is no split for this row to exercise"
    );

    assert_projected_past_the_turn_boundary(&base, &controlled, "controlled");
}

/// Paired positive reach-guard for row 1: the same capture with no turn control
/// must reach the same horizon under the same assertions. Without it an
/// unprojectable fixture would red for the wrong reason, and go green on any
/// change that made `project_to` return early.
#[test]
fn projection_reaches_the_same_horizon_without_turn_control() {
    let base = load_2p_capture();
    assert!(
        base.turn_decision_controller.is_none(),
        "the capture must carry no turn control, or this is not the OFF leg"
    );
    assert_eq!(
        authorized_submitter_for_player(&base, SEAT),
        SEAT,
        "with no controller the seat submits for itself"
    );

    assert_projected_past_the_turn_boundary(&base, &base, "uncontrolled");
}

/// Row 2. The mapping must be conditional, not a blanket redirect: on a real
/// four-player board only the controlled seat resolves to the controller. The
/// bystander is the admitted-member hunt — a seat the class must refuse.
#[test]
fn the_submitter_mapping_redirects_only_the_controlled_seat() {
    let base = load_4p_capture();
    let seat = base.active_player;
    let controller = PlayerId(0);
    let bystander = PlayerId(1);
    let controlled = with_turn_control(&base, controller);
    assert_control_is_discriminating(&controlled, seat, controller);
    assert_ne!(
        bystander, seat,
        "the bystander must not be the controlled seat"
    );
    assert_ne!(
        bystander, controller,
        "the bystander must not be the controller"
    );

    assert_eq!(
        authorized_submitter_for_player(&controlled, seat),
        controller,
        "CR 723.5: the controlled seat's decisions go to the controller"
    );
    assert_eq!(
        authorized_submitter_for_player(&controlled, bystander),
        bystander,
        "CR 723.5 scopes control to the controlled player, so an uncontrolled \
         bystander still submits for itself"
    );
    assert_eq!(
        authorized_submitter_for_player(&controlled, controller),
        controller,
        "the controller submits for itself"
    );
}

/// Row 3. CR 723.9: an effect may give a player control of themselves, and then
/// that player makes their own decisions — the split must collapse back to a
/// no-op rather than reject.
#[test]
fn self_control_leaves_the_seat_as_its_own_submitter() {
    let base = load_2p_capture();
    let controlled = with_turn_control(&base, SEAT);
    assert_eq!(
        controlled.turn_decision_controller,
        Some(SEAT),
        "self-control must actually be installed"
    );
    assert_eq!(
        authorized_submitter_for_player(&controlled, SEAT),
        SEAT,
        "CR 723.9: a player controlling themselves submits their own decisions"
    );

    assert_projected_past_the_turn_boundary(&base, &controlled, "self-controlled");
}

const OPPOSITION_AGENT: &str = "Flash\n\
You control your opponents while they're searching their libraries.\n\
While an opponent is searching their library, they exile each card they find. You may play those cards for as long as they remain exiled, and you may spend mana as though it were mana of any color to cast them.";
const TEST_TUTOR: &str =
    "Search your library for a card, put that card into your hand, then shuffle.";

/// A board halted on the searcher's own library search, optionally under an
/// Opposition Agent. CR 723.2 latches the search decision onto the agent's
/// controller without ever setting `turn_decision_controller`.
fn opposition_agent_search_state(with_agent: bool) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_agent {
        scenario.add_creature_from_oracle(CONTROLLER, "Opposition Agent", 3, 2, OPPOSITION_AGENT);
    }
    let tutor = scenario
        .add_spell_to_hand_from_oracle(SEAT, "Test Tutor", false, TEST_TUTOR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let found = scenario
        .add_spell_to_library_top(SEAT, "Found Card", true)
        .id();
    scenario.add_card_to_library_top(SEAT, "Library Filler");
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = SEAT;
        state.priority_player = SEAT;
        state.waiting_for = WaitingFor::Priority { player: SEAT };
    }
    let outcome = runner.cast(tutor).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::SearchChoice { player, .. } if *player == SEAT
        ),
        "the tutor must halt on the searcher's own search choice, got {:?}",
        outcome.final_waiting_for()
    );
    (runner.state().clone(), found)
}

fn agent_permission_granted_to(state: &GameState, found: ObjectId, grantee: PlayerId) -> bool {
    state.objects[&found]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to,
                    mana_spend_permission: Some(ManaSpendPermission::AnyColor),
                    ..
                } if *granted_to == grantee
            )
        })
}

/// Row 4. The second CR 723 redirect source. `turn_decision_controller` is
/// unset here, so a projection that only handled that field would dispatch the
/// searcher as its own submitter and be refused.
#[test]
fn projection_advances_over_a_search_control_latch() {
    let (latched, found) = opposition_agent_search_state(true);
    assert!(
        latched.turn_decision_controller.is_none(),
        "the latch must redirect with no turn-control field set"
    );
    assert_eq!(
        authorized_submitter_for_player(&latched, SEAT),
        CONTROLLER,
        "the searcher's decisions are submittable only by the agent's controller"
    );

    let projection = project_to(
        &latched,
        CONTROLLER,
        SEAT,
        ProjectionHorizon::OpponentBeginCombat,
        Deadline::none(),
    )
    .unwrap_or_else(|bail| panic!("latched: projection bailed with {bail:?}"));

    // The split is what carries this row: the collapsed dispatch is refused on
    // this same board, which is the bail a re-collapsed `project_to` would hit.
    let mut collapsed = latched.clone();
    assert!(
        matches!(
            apply_for_simulation(
                &mut collapsed,
                SEAT,
                GameAction::SelectCards { cards: vec![found] }
            ),
            Err(EngineError::WrongPlayer)
        ),
        "the searcher is not its own authorized submitter under the latch"
    );

    assert_eq!(
        projection.state.objects[&found].zone,
        Zone::Exile,
        "CR 723.3: the found card follows the searcher's own replaced destination"
    );
    assert!(
        agent_permission_granted_to(&projection.state, found, CONTROLLER),
        "the agent grants its controller permission to play the exiled card"
    );
}

/// Paired reach-guard: the same tutor with no agent reaches the same horizon,
/// so the row above pins the latch rather than the projection's ability to
/// traverse a search at all.
#[test]
fn projection_reaches_the_same_horizon_without_a_search_control_latch() {
    let (unlatched, found) = opposition_agent_search_state(false);
    assert_eq!(
        authorized_submitter_for_player(&unlatched, SEAT),
        SEAT,
        "with no agent the searcher submits its own search choice"
    );

    let projection = project_to(
        &unlatched,
        CONTROLLER,
        SEAT,
        ProjectionHorizon::OpponentBeginCombat,
        Deadline::none(),
    )
    .unwrap_or_else(|bail| panic!("unlatched: projection bailed with {bail:?}"));

    assert_eq!(
        projection.state.objects[&found].zone,
        Zone::Hand,
        "with no agent the found card reaches the tutor's own destination"
    );
    assert!(!agent_permission_granted_to(
        &projection.state,
        found,
        CONTROLLER
    ));
}

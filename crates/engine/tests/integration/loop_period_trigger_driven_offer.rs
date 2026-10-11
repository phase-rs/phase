//! CR 732.2a/b/c: a trigger-driven recorded period owned by the priority holder is offered at the
//! window where its recurrence stands on top of the stack, and a confirmed proposal replays it.
//!
//! Board A is `abdel_adrian_animate_dead_altar_board`'s and Board B is
//! `loop_period_trigger_driven_arming::build_board_b`'s, both driven through `apply()` and never
//! rebuilt. Both answer the CR 603.3b ordering prompt with `altar_resolves_first`, except where a
//! row names `identity_order`.

use std::collections::BTreeMap;
use std::path::Path;

use engine::analysis::decision_template::{
    declaration_conforms, validate_recorded_answers, AimContext, ChoicePoint, DecisionSlot,
    DecisionTemplate, IterationCount, MayChoiceOption, PinValidation, PinnedDecision, TargetPin,
};
use engine::analysis::loop_check::{OfferRoad, ShortcutResponse};
use engine::analysis::resource::{loop_detect_cost, reset_loop_detect_cost, LoopDetectCost};
use engine::game::scenario::{GameRunner, P0};
use engine::game::NamingCause;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, StackEntryKind, WaitingFor, YieldTarget,
};
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

use crate::loop_period_accessor_answers::altar_resolves_first;
use crate::period_confirm_rows::{published_to_p0, shortcut_schema};

/// Beat cap for every drive here; it bounds a runaway drive and is read by no assertion.
const BEAT_CAP: usize = 400;

/// How many of the board's voluntary choices a drive accepts before it declines.
const ACCEPTS: usize = 8;

#[derive(Debug, Clone, Copy)]
enum Board {
    A,
    B,
    /// Board B with Soul's Attendant beside it, whose "may" every drive takes.
    BAttendant,
}

const BOARDS: [Board; 2] = [Board::A, Board::B];

type Policy = Box<dyn FnMut(&GameState) -> Option<GameAction>>;

type Ordering = fn(&GameState) -> Option<GameAction>;

struct Drive {
    runner: GameRunner,
    minting: ObjectId,
    order: Ordering,
    policy: Policy,
}

/// One `apply()`: the detector cost it paid.
struct Apply {
    cost: LoopDetectCost,
}

fn cast_animate_dead(runner: &mut GameRunner, animate_dead: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&animate_dead].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: animate_dead,
            card_id,
            targets: vec![target],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Animate Dead is castable from the built board");
}

/// CR 603.3b: every ordering prompt answered with the identity permutation.
fn identity_order(state: &GameState) -> Option<GameAction> {
    let WaitingFor::OrderTriggers { triggers, .. } = &state.waiting_for else {
        return None;
    };
    Some(GameAction::OrderTriggers {
        order: (0..triggers.len()).collect(),
    })
}

fn start(board: Board) -> Option<Drive> {
    start_ordered(board, altar_resolves_first)
}

/// Board A exiles only Animate Dead at Abdel Adrian's exile while accepts remain; Board B aims
/// each enters trigger at Felidar Guardian and accepts its exile while accepts remain.
fn start_ordered(board: Board, order: Ordering) -> Option<Drive> {
    match board {
        Board::A => {
            let built = crate::abdel_adrian_animate_dead_altar_board::build()?;
            let mut runner = built.runner;
            runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
            let returned = runner.state().objects[&built.animate_dead].card_id;
            cast_animate_dead(&mut runner, built.animate_dead, built.abdel);
            let mut left = ACCEPTS;
            Some(Drive {
                runner,
                minting: built.abdel,
                order,
                policy: Box::new(move |state| {
                    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
                        return None;
                    };
                    let exiled = if left > 0 {
                        left -= 1;
                        cards
                            .iter()
                            .copied()
                            .filter(|id| {
                                state
                                    .objects
                                    .get(id)
                                    .is_some_and(|object| object.card_id == returned)
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Some(GameAction::SelectCards { cards: exiled })
                }),
            })
        }
        Board::B | Board::BAttendant => {
            let (built, attendant) = match board {
                Board::BAttendant => {
                    let (built, attendant) = build_board_b_with_attendant()?;
                    (built, Some(attendant))
                }
                _ => (
                    crate::loop_period_trigger_driven_arming::build_board_b()?,
                    None,
                ),
            };
            let mut runner = built.runner;
            cast_animate_dead(&mut runner, built.animate_dead, built.felidar);
            let felidar = built.felidar;
            let mut left = ACCEPTS;
            Some(Drive {
                runner,
                minting: built.preston,
                order,
                policy: Box::new(move |state| {
                    if matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { source_id, .. }
                        if Some(source_id) == attendant)
                    {
                        return Some(GameAction::DecideOptionalEffect { accept: true });
                    }
                    let legal = engine::ai_support::legal_actions(state);
                    if left > 0 {
                        if let Some(aimed) = legal.iter().find(|action| {
                            matches!(action, GameAction::ChooseTarget {
                                target: Some(TargetRef::Object(id)),
                            } if *id == felidar)
                        }) {
                            return Some(aimed.clone());
                        }
                    }
                    let accept = left > 0;
                    let decided = legal.iter().find(|action| {
                        matches!(action, GameAction::DecideOptionalEffect { accept: answered }
                            if *answered == accept)
                    })?;
                    if accept {
                        left -= 1;
                    }
                    Some(decided.clone())
                }),
            })
        }
    }
}

fn top_trigger_source(state: &GameState) -> Option<ObjectId> {
    match &state.stack.back()?.kind {
        StackEntryKind::TriggeredAbility { source_id, .. } => Some(*source_id),
        _ => None,
    }
}

/// A `Priority{P0}` frame whose top entry is the minting trigger, with a span its return to the
/// top named.
fn recurrence_window(state: &GameState, minting: ObjectId) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        && top_trigger_source(state) == Some(minting)
        && engine::game::play_trace_view(state).is_some_and(|view| {
            view.named
                .iter()
                .any(|span| span.cause == NamingCause::TriggerTop)
        })
}

fn is_offer(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::LoopShortcut { .. })
}

fn token_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|object| object.is_token))
        .count()
}

type PlayerZones = (i32, Vec<ObjectId>, Vec<ObjectId>, Vec<ObjectId>);

type BoardFingerprint = (
    BTreeMap<ObjectId, (Zone, bool)>,
    Vec<PlayerZones>,
    Vec<ObjectId>,
    Vec<ObjectId>,
);

/// Every object's zone and tapped state, each player's life, library, hand and graveyard, the
/// stack, and the battlefield.
fn board_fingerprint(state: &GameState) -> BoardFingerprint {
    (
        state
            .objects
            .values()
            .map(|object| (object.id, (object.zone, object.tapped)))
            .collect(),
        state
            .players
            .iter()
            .map(|player| {
                (
                    player.life,
                    player.library.iter().copied().collect(),
                    player.hand.iter().copied().collect(),
                    player.graveyard.iter().copied().collect(),
                )
            })
            .collect(),
        state.stack.iter().map(|entry| entry.id).collect(),
        state.battlefield.iter().copied().collect(),
    )
}

impl Drive {
    fn state(&self) -> &GameState {
        self.runner.state()
    }

    fn next_action(&mut self) -> GameAction {
        let state = self.runner.state();
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return GameAction::PassPriority;
        }
        (self.order)(state)
            .or_else(|| (self.policy)(state))
            .unwrap_or_else(|| {
                engine::ai_support::legal_actions(state)
                    .into_iter()
                    .find(|action| !matches!(action, GameAction::PassPriority))
                    .expect("a prompt the declared drive does not answer offers a legal action")
            })
    }

    fn apply(&mut self, action: GameAction) -> Apply {
        reset_loop_detect_cost();
        self.runner
            .act(action.clone())
            .unwrap_or_else(|error| panic!("{action:?} was rejected: {error:?}"));
        Apply {
            cost: loop_detect_cost(),
        }
    }

    /// Drive until `apply()` returns an offer, a recurrence window, or the end of the game.
    fn drive_to_window(&mut self) -> Vec<Apply> {
        let mut applies = Vec::new();
        for _ in 0..BEAT_CAP {
            let action = self.next_action();
            applies.push(self.apply(action));
            let state = self.state();
            if is_offer(state)
                || recurrence_window(state, self.minting)
                || matches!(state.waiting_for, WaitingFor::GameOver { .. })
            {
                return applies;
            }
        }
        panic!("the drive reached no window in {BEAT_CAP} beats");
    }

    /// Declare `count` with no template and accept from every responder.
    fn take(&mut self, count: IterationCount) -> Vec<Apply> {
        self.take_declared(count, None)
    }

    /// Declare `count` with `template` and accept from every responder.
    fn take_declared(
        &mut self,
        count: IterationCount,
        template: Option<DecisionTemplate>,
    ) -> Vec<Apply> {
        let mut applies = vec![self.apply(GameAction::DeclareShortcut { count, template })];
        assert!(
            matches!(
                self.state().waiting_for,
                WaitingFor::RespondToShortcut { .. }
            ),
            "the declaration opens the response window; got {}",
            self.state().waiting_for.variant_name()
        );
        while matches!(
            self.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ) {
            applies.push(self.apply(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            }));
        }
        applies
    }
}

/// Each board driven to the first window `apply()` returns at, which must be its offer.
fn offered(board: Board) -> Option<(Drive, Vec<Apply>)> {
    let mut drive = start(board)?;
    let mut applies = drive.drive_to_window();
    // Soul's Attendant's trigger lands above Preston's, so that board passes one recurrence window
    // before its offer stands.
    if let Board::BAttendant = board {
        if !is_offer(drive.state()) {
            let action = drive.next_action();
            applies.push(drive.apply(action));
            applies.extend(drive.drive_to_window());
        }
    }
    let state = drive.state();
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { proposer, road: OfferRoad::RecordedPeriod, .. }
                if proposer == P0
        ),
        "{board:?}: the first recurrence window is a recorded-period offer to P0; got {} with \
         top {:?}",
        state.waiting_for.variant_name(),
        top_trigger_source(state)
    );
    match board {
        // CR 117.3b/c: a span a repeat names is offered at the first priority window after the
        // repeated resolution, before the minting trigger is back on top.
        Board::A => {
            assert_eq!(
                engine::game::play_trace_view(state)
                    .and_then(|view| view.offered)
                    .map(|span| span.cause),
                Some(NamingCause::Repeat),
                "A: the offered span is the one a repeat named"
            );
            assert_ne!(
                top_trigger_source(state),
                Some(drive.minting),
                "A: the offer stands before the minting trigger's recurrence window"
            );
        }
        Board::BAttendant => {}
        Board::B => assert_eq!(
            top_trigger_source(state),
            Some(drive.minting),
            "B: the offer stands at the window where the minting trigger is on top"
        ),
    }
    Some((drive, applies))
}

fn offer_cost(board: Board, applies: &[Apply]) -> LoopDetectCost {
    let cost = applies.last().expect("the offer apply").cost;
    assert_eq!(
        cost.object_growth_calls, 1,
        "{board:?}: the production producer minted the offer on this apply"
    );
    cost
}

/// CR 732.2a + CR 732.2b + CR 732.2c: each board is offered at its first recurrence window, and
/// taking a fixed count replays the period.
#[test]
fn a_trigger_driven_period_is_offered_at_its_first_recurrence_window_and_taken() {
    for board in BOARDS {
        let Some((mut drive, applies)) = offered(board) else {
            return;
        };
        offer_cost(board, &applies);
        let (tokens, libraries): (usize, Vec<usize>) = (
            token_count(drive.state()),
            drive
                .state()
                .players
                .iter()
                .map(|player| player.library.len())
                .collect(),
        );
        drive.take(IterationCount::Fixed(3));
        let state = drive.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "{board:?}: the take ends at priority; got {}",
            state.waiting_for.variant_name()
        );
        assert!(
            token_count(state) > tokens,
            "{board:?}: the take minted tokens ({tokens} -> {})",
            token_count(state)
        );
        for (seat, player) in state.players.iter().enumerate().skip(1) {
            assert!(
                player.library.len() < libraries[seat],
                "{board:?}: seat {seat}'s library was milled by the take"
            );
        }
    }

    let restored = crate::committed_dump_walk::restore_committed(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/combo_infinite_pile_4p_offer.json.gz"),
    )
    .expect("the committed offer restores");
    assert!(
        matches!(
            restored.waiting_for,
            WaitingFor::LoopShortcut {
                road: OfferRoad::RecordedPeriod,
                ..
            }
        ),
        "live control: a priority-driven recorded-period offer restores as one"
    );
}

/// The names of the sources of the stack entries beneath the top one.
fn sources_beneath_top(state: &GameState) -> Vec<String> {
    state
        .stack
        .iter()
        .take(state.stack.len().saturating_sub(1))
        .map(|entry| {
            state
                .objects
                .get(&entry.source_id)
                .map_or_else(String::new, |object| object.name.clone())
        })
        .collect()
}

fn all_altar_triggers(sources: &[String]) -> bool {
    sources.iter().all(|name| name == "Altar of the Brood")
}

/// CR 603.3b + CR 732.2a + CR 732.2c: under identity ordering Board A's Altar of the Brood
/// triggers accumulate beneath the recurrence, and the period is offered and taken.
#[test]
fn an_accumulating_stack_beneath_the_recurrence_is_offered_and_taken() {
    let Some(mut drive) = start_ordered(Board::A, identity_order) else {
        return;
    };
    let mut offer_apply = None;
    for _ in 0..BEAT_CAP {
        let action = drive.next_action();
        let apply = drive.apply(action);
        if is_offer(drive.state()) {
            offer_apply = Some(apply);
            break;
        }
        if matches!(drive.state().waiting_for, WaitingFor::GameOver { .. }) {
            break;
        }
    }
    let offer_apply =
        offer_apply.expect("identity-ordered Board A is offered before the game ends");
    let state = drive.state();
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { proposer, road: OfferRoad::RecordedPeriod, .. }
                if proposer == P0
        ),
        "the offer is a recorded-period offer to P0"
    );
    assert_eq!(offer_apply.cost.object_growth_calls, 1);
    let beneath = sources_beneath_top(state);
    assert!(
        !beneath.is_empty() && all_altar_triggers(&beneath),
        "reach guard: Altar of the Brood triggers accumulate beneath the recurrence; {beneath:?}"
    );
    let tokens = token_count(state);
    let libraries: Vec<usize> = state
        .players
        .iter()
        .map(|player| player.library.len())
        .collect();

    drive.take(IterationCount::Fixed(3));
    let state = drive.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the take ends at priority; got {}",
        state.waiting_for.variant_name()
    );
    assert_eq!(token_count(state), tokens + 3);
    for (seat, player) in state.players.iter().enumerate().skip(1) {
        assert_eq!(
            player.library.len(),
            libraries[seat],
            "seat {seat}'s mills wait beneath the recurrence"
        );
    }
    let after = sources_beneath_top(state);
    assert!(
        after.len() > beneath.len() && all_altar_triggers(&after),
        "the take left more Altar of the Brood triggers beneath the recurrence; {after:?}"
    );
    assert!(
        engine::game::play_trace_view(state).is_none_or(|view| view.offered.is_none()),
        "the take leaves no offer standing in the trace"
    );
}

/// CR 117.3b/c + CR 608.1: Board A is offered before its recurrence window, for the period its base
/// offer performs.
#[test]
fn board_a_is_offered_early_for_its_base_period() {
    let Some((drive, _)) = offered(Board::A) else {
        return;
    };
    let performed = engine::game::period_confirm::performed_for_tests(drive.state())
        .expect("reach: the offer names its span")
        .expect("the offered period replays");
    assert!(
        crate::loop_period_performs::same_up_to_rotation(
            &performed,
            &crate::loop_period_performs::BOARD_A_PERIOD
        ),
        "{performed:?}"
    );
}

/// CR 603.3d + CR 732.2a: Board B is offered at the window where Preston's trigger is back on top,
/// for the cycle it performed, and the offer asks the cycle's announced targets.
#[test]
fn board_b_is_offered_at_its_base_window_with_its_target_answers() {
    let Some((drive, _)) = offered(Board::B) else {
        return;
    };
    let state = drive.state();
    assert_eq!(
        engine::game::play_trace_view(state)
            .and_then(|view| view.offered)
            .map(|span| span.cause),
        Some(NamingCause::TriggerTop)
    );
    assert_eq!(
        engine::game::period_confirm::performed_for_tests(state),
        Some(Ok(vec![
            "Preston, the Vanisher".to_string(),
            "Altar of the Brood".to_string(),
            "Felidar Guardian".to_string(),
            "Altar of the Brood".to_string(),
            "Felidar Guardian".to_string(),
        ])),
        "one replayed cycle resolves Preston's trigger, then each Illusion's"
    );
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        unreachable!("offered() asserted the offer");
    };
    assert!(
        schema
            .points
            .iter()
            .any(|point| point.slot.point == ChoicePoint::AnnouncedTarget),
        "{:?}",
        schema.points
    );
}

/// CR 732.2a + CR 732.2c: an until-lethal proposal on a trigger-driven offer, whose mint names no
/// winner, ends at priority with the board untouched and the proposer's trace discarded.
#[test]
fn an_until_lethal_take_of_a_trigger_driven_offer_changes_nothing() {
    let Some((mut drive, _)) = offered(Board::A) else {
        return;
    };
    let offer = board_fingerprint(drive.state());
    drive.take(IterationCount::UntilLethal);
    let state = drive.state();
    let WaitingFor::Priority { player } = state.waiting_for else {
        panic!(
            "the fallback ends at priority; got {}",
            state.waiting_for.variant_name()
        );
    };
    assert!(
        state
            .players
            .iter()
            .any(|seat| seat.id == player && !seat.is_eliminated),
        "priority goes to a player still in the game"
    );
    assert_eq!(board_fingerprint(state), offer, "the board is the offer's");
    assert!(
        engine::game::play_trace_view(state).is_none_or(|view| view
            .entries
            .iter()
            .all(|entry| entry.seat != P0
                || matches!(entry.kind, engine::game::EntryKind::Answer { .. }))),
        "the fallback discards the proposer's plays and resolutions"
    );
}

/// Each apply evaluates the loop shortcut at most once, through the take and on to the next window
/// whose cover the producer asks.
#[test]
fn each_apply_evaluates_the_loop_shortcut_at_most_once() {
    for board in BOARDS {
        let Some((mut drive, mut applies)) = offered(board) else {
            return;
        };
        applies.extend(drive.take(IterationCount::Fixed(3)));
        let next = loop {
            let action = drive.next_action();
            let apply = drive.apply(action);
            let asked = apply.cost.object_growth_calls > 0;
            applies.push(apply);
            if asked {
                break applies.last().expect("just pushed").cost;
            }
            assert!(
                applies.len() < BEAT_CAP,
                "{board:?}: no later window asked the producer"
            );
        };
        assert_eq!(
            next.object_growth_calls, 1,
            "{board:?}: the post-take window asks the producer once"
        );
        if let Board::A = board {
            assert!(
                is_offer(drive.state()),
                "Board A: the post-take window is offered again; got {}",
                drive.state().waiting_for.variant_name()
            );
        }
        for (index, apply) in applies.iter().enumerate() {
            assert!(
                apply.cost.object_growth_calls <= 1 && apply.cost.reconcile_calls <= 1,
                "{board:?}: apply {index} evaluated the shortcut more than once; {:?}",
                apply.cost
            );
        }
    }
}

/// The ring road is asked first and the recorded road mints only while the ring leaves priority
/// standing; a ring mint over a trigger-driven record keeps the ring road.
#[test]
fn the_ring_road_is_asked_before_the_recorded_road() {
    let Some((_, applies)) = offered(Board::B) else {
        return;
    };
    assert_eq!(
        applies
            .last()
            .expect("the offer apply")
            .cost
            .reconcile_calls,
        1,
        "Board B: the ring bridge was entered and refused before the recorded road minted"
    );

    let mut state = crate::committed_dump_walk::restore_committed(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fantastic_four_bounded_loop_4p.json.gz"),
    )
    .expect("the committed board restores");
    let mut aimed_at = None;
    for _ in 0..BEAT_CAP {
        reset_loop_detect_cost();
        crate::loop_shortcut_drain_boards::drive_one_beat(&mut state, &mut aimed_at);
        if let WaitingFor::LoopShortcut { road, .. } = state.waiting_for {
            assert_eq!(
                road,
                OfferRoad::Ring,
                "the bounded-cycle mint is the ring road's"
            );
            assert_eq!(
                loop_detect_cost().object_growth_calls,
                0,
                "the recorded road is not asked once the ring road minted"
            );
            return;
        }
    }
    panic!("the committed board reached no offer in {BEAT_CAP} beats");
}

/// Offers `board` and takes it at `n`, metering only the take.
fn metered_take(board: Board, n: u32) -> GameState {
    let (mut drive, _) = offered(board).expect("the board builds from the card fixture");
    engine::game::perf_counters::reset();
    drive.take(IterationCount::Fixed(n));
    drive.state().clone()
}

/// Board A's take: its per-cycle history work does not grow with its count.
#[test]
fn trigger_driven_take_history_work_is_flat_per_cycle_a() {
    use crate::loop_shortcut::{
        assert_take_history_work_is_flat, TakeHistoryMap, TakeHistoryVector,
    };

    assert_take_history_work_is_flat(
        32,
        &[
            TakeHistoryVector::JournalEntries,
            TakeHistoryVector::BattlefieldEntries,
        ],
        &[
            TakeHistoryMap::AbilityResolutions,
            TakeHistoryMap::TrackedObjectSets,
            TakeHistoryMap::TrackedSetMemberCauses,
        ],
        |n| metered_take(Board::A, n),
    );
}

/// Board B's take: its per-cycle history work does not grow with its count.
#[test]
fn trigger_driven_take_history_work_is_flat_per_cycle_b() {
    use crate::loop_shortcut::{
        assert_take_history_work_is_flat, TakeHistoryMap, TakeHistoryVector,
    };

    assert_take_history_work_is_flat(
        32,
        &[
            TakeHistoryVector::JournalEntries,
            TakeHistoryVector::BattlefieldEntries,
        ],
        &[TakeHistoryMap::AbilityResolutions],
        |n| metered_take(Board::B, n),
    );
}

/// Board B with Soul's Attendant on P0's battlefield: "Whenever another creature enters, you may
/// gain 1 life." Its "may" gains life and never drives the recurrence.
fn build_board_b_with_attendant() -> Option<(
    crate::loop_period_trigger_driven_arming::PrestonBoard,
    ObjectId,
)> {
    use engine::game::scenario::GameScenario;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::mana::{ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;

    let db = crate::support::shared_card_db()?;
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let preston = scenario.add_real_card(P0, "Preston, the Vanisher", Zone::Battlefield, db);
    let altar = scenario.add_real_card(P0, "Altar of the Brood", Zone::Battlefield, db);
    let attendant = scenario.add_real_card(P0, "Soul's Attendant", Zone::Battlefield, db);
    let felidar = scenario.add_real_card(P0, "Felidar Guardian", Zone::Graveyard, db);
    let animate_dead = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    }
    for seat in (0..4).map(PlayerId) {
        for _ in 0..40 {
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
    Some((
        crate::loop_period_trigger_driven_arming::PrestonBoard {
            runner,
            felidar,
            animate_dead,
            preston,
            altar,
        },
        attendant,
    ))
}

/// The offer standing in `state` declared with every published point pinned the way its confirmed
/// period answered it, except each "may" `flip` names, which is answered the other way.
fn recorded_declaration(
    state: &GameState,
    flip: impl Fn(&DecisionSlot) -> bool,
) -> DecisionTemplate {
    let WaitingFor::LoopShortcut {
        proposer,
        schema,
        period,
        ..
    } = &state.waiting_for
    else {
        panic!("an offer stands");
    };
    let decisions: Vec<PinnedDecision> = schema
        .points
        .iter()
        .map(|point| {
            let recorded = period
                .choices()
                .iter()
                .find(|pin| pin.slot() == &point.slot)
                .expect("every published point is a recorded answer")
                .clone();
            match recorded {
                PinnedDecision::MayChoice { slot, take } if flip(&slot) => {
                    let take = match take {
                        MayChoiceOption::Take => MayChoiceOption::Decline,
                        MayChoiceOption::Decline => MayChoiceOption::Take,
                    };
                    PinnedDecision::MayChoice { slot, take }
                }
                // CR 400.7: the declaration names each target at its live incarnation.
                PinnedDecision::Targets { slot, targets } => PinnedDecision::Targets {
                    slot,
                    targets: targets
                        .into_iter()
                        .map(|pin| match pin {
                            TargetPin::ByIdentity(YieldTarget::ThisObject {
                                source_id,
                                trigger_description,
                                ..
                            }) => TargetPin::ByIdentity(YieldTarget::ThisObject {
                                source_id,
                                incarnation: None,
                                trigger_description,
                            }),
                            other => other,
                        })
                        .collect(),
                },
                other => other,
            }
        })
        .collect();
    DecisionTemplate {
        owner: *proposer,
        key: engine::analysis::decision_template::DecisionGroupKey::from_sources(
            &decisions
                .iter()
                .map(|pin| pin.slot().source.clone())
                .collect::<Vec<_>>(),
            engine::analysis::decision_template::DecisionKind::LoopChoice,
        ),
        decisions,
        replay: engine::analysis::decision_template::ReplayMode::Static,
    }
}

/// CR 732.2a + CR 603.5: on `board`'s recorded-period offer, a declaration answering a "may"
/// `flip` names otherwise than the confirmed period did is refused into the manual-play handback,
/// and the declaration of the recorded answers is admitted and taken. `taken` is asserted of the
/// state after the take.
fn assert_the_declared_may_answer_must_be_the_recorded_one(
    board: Board,
    flip: impl Fn(&GameState, &DecisionSlot) -> bool,
    taken: impl Fn(&GameState, &GameState),
) {
    const COUNT: u32 = 3;
    let Some((mut drive, _)) = offered(board) else {
        return;
    };
    let offer = drive.state().clone();
    let WaitingFor::LoopShortcut {
        proposer,
        schema,
        certificate,
        declaration,
        period,
        ..
    } = &offer.waiting_for
    else {
        unreachable!("offered() asserted the offer");
    };
    let flipped: Vec<&PinnedDecision> = period
        .choices()
        .iter()
        .filter(|pin| matches!(pin, PinnedDecision::MayChoice { slot, take: MayChoiceOption::Take } if flip(&offer, slot)))
        .collect();
    assert!(
        !flipped.is_empty(),
        "{board:?}: reach guard: the period recorded a taken \"may\" the row flips; {:?}",
        period.choices()
    );
    let declined = recorded_declaration(&offer, |slot| flip(&offer, slot));
    let recorded = recorded_declaration(&offer, |_| false);
    let conforms = |template: &DecisionTemplate, recorded: Option<&[PinnedDecision]>| {
        declaration_conforms(
            schema,
            template,
            COUNT,
            AimContext {
                proposer: *proposer,
                per_cycle: certificate.per_cycle.as_ref(),
                published: declaration.as_ref(),
            },
            recorded,
            &offer,
        )
    };
    assert!(
        conforms(&declined, None),
        "{board:?}: reach guard: the declined declaration answers every published point legally"
    );
    assert!(
        !conforms(&declined, period.recorded_answers()),
        "{board:?}: the shared authority refuses an answer the confirmed period did not record"
    );
    assert!(
        conforms(&recorded, period.recorded_answers()),
        "{board:?}: the shared authority admits the confirmed period's own answers"
    );
    assert_eq!(
        validate_recorded_answers(&declined, period.choices(), COUNT, &offer),
        Err(PinValidation::DivergesFromRecordedPeriod {
            slot: flipped[0].slot().clone()
        }),
        "{board:?}: the refusal names the slot answered otherwise than the period"
    );
    assert_eq!(
        validate_recorded_answers(&recorded, period.choices(), COUNT, &offer),
        Ok(())
    );

    let mut refused = GameRunner::from_state(offer.clone());
    refused
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(COUNT),
            template: Some(declined),
        })
        .expect("a refused declaration hands back rather than erroring");
    assert!(
        matches!(refused.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{board:?}: the declined declaration is handed back to manual play; got {}",
        refused.state().waiting_for.variant_name()
    );
    assert_eq!(
        token_count(refused.state()),
        token_count(&offer),
        "{board:?}: nothing was taken"
    );

    drive.take_declared(IterationCount::Fixed(COUNT), Some(recorded));
    let state = drive.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{board:?}: the recorded declaration is taken and ends at priority; got {}",
        state.waiting_for.variant_name()
    );
    assert!(
        token_count(state) > token_count(&offer),
        "{board:?}: the take replayed the period"
    );
    taken(&offer, state);
}

/// CR 732.2a + CR 603.5: Felidar Guardian's "you may exile another target permanent you control"
/// drives Board B's recurrence; declaring it declined is refused, declaring it taken is admitted.
#[test]
fn a_declared_recurrence_may_must_be_the_confirmed_periods_answer() {
    assert_the_declared_may_answer_must_be_the_recorded_one(
        Board::B,
        |_, slot| slot.point == ChoicePoint::MayGate,
        |_, _| {},
    );
}

/// CR 732.2a + CR 603.5: Soul's Attendant's "you may gain 1 life" does not drive the recurrence;
/// declaring it declined is still refused, and the take of the recorded answer gains the life.
#[test]
fn a_declared_optional_may_outside_the_recurrence_must_be_the_confirmed_periods_answer() {
    let attendant_slot = |state: &GameState, slot: &DecisionSlot| {
        slot.point == ChoicePoint::MayGate && asked_of_attendant(state, slot)
    };
    assert_the_declared_may_answer_must_be_the_recorded_one(
        Board::BAttendant,
        attendant_slot,
        |offer, state| {
            assert!(
                state.players[0].life > offer.players[0].life,
                "the take replayed Soul's Attendant's recorded life gain ({} -> {})",
                offer.players[0].life,
                state.players[0].life
            );
        },
    );
}

/// CR 732.2a + CR 603.5: the recorded offer `drive` stands on states each "may" as fixed. A
/// response pinning one is refused and the offer stands; a response pinning none declares the
/// confirmed period's own answers, and its take replays the period.
fn assert_recorded_mays_are_fixed_at_the_interaction_ingress(mut drive: Drive) {
    use engine::game::interaction::{resolve_interaction_response, submit_interaction};
    use engine::types::interaction::{
        InteractionReasonCode, InteractionResponse, InteractionShortcutDecision,
        InteractionShortcutPin, InteractionShortcutPointKind, InteractionSubmission,
    };

    let WaitingFor::LoopShortcut { period, .. } = &drive.state().waiting_for else {
        panic!("an offer stands");
    };
    let recorded_mays: Vec<PinnedDecision> = period
        .choices()
        .iter()
        .filter(|pin| matches!(pin, PinnedDecision::MayChoice { .. }))
        .cloned()
        .collect();
    let (offer, view) = published_to_p0(drive.state());
    let (opportunity, points) =
        shortcut_schema(&view).expect("reach guard: the offer is published as a shortcut schema");
    let mays: Vec<_> = points
        .iter()
        .filter(|point| point.kind == InteractionShortcutPointKind::MayChoice)
        .collect();
    assert_eq!(
        mays.len(),
        recorded_mays.len(),
        "reach guard: every recorded \"may\" is a published point"
    );
    assert!(
        !mays.is_empty(),
        "reach guard: the period recorded a \"may\""
    );
    for may in &mays {
        assert!(
            may.read_only && may.candidate_ids.is_empty() && (may.min, may.max) == (0, 0),
            "a recorded offer's \"may\" offers no answer; got read_only={} candidates={} min={} \
             max={}",
            may.read_only,
            may.candidate_ids.len(),
            may.min,
            may.max
        );
    }

    // One pin per answerable point, each by its only candidate.
    let answerable: Vec<InteractionShortcutPin> = points
        .iter()
        .filter(|point| !point.read_only)
        .map(|point| InteractionShortcutPin {
            group: point.group,
            choice_ids: vec![point.candidate_ids[0].clone()],
            amounts: Vec::new(),
        })
        .collect();
    assert!(
        !answerable.is_empty(),
        "reach guard: the response reaches the pin decoder"
    );
    let submission = |pins: Vec<InteractionShortcutPin>| InteractionSubmission {
        interaction_id: opportunity.interaction_id.clone(),
        response: InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::Fixed { iterations: 3 },
            pins,
        },
    };

    let mut pinned = answerable.clone();
    pinned.push(InteractionShortcutPin {
        group: mays[0].group,
        choice_ids: vec![answerable[0].choice_ids[0].clone()],
        amounts: Vec::new(),
    });
    let mut refused = offer.clone();
    assert_eq!(
        submit_interaction(&mut refused, P0, submission(pinned))
            .map(|_| ())
            .map_err(|error| error.code),
        Err(InteractionReasonCode::ConstraintUnsatisfied),
        "a response pinning a fixed \"may\" is refused by reason"
    );
    assert!(
        is_offer(&refused),
        "the refused response leaves the offer standing; got {}",
        refused.waiting_for.variant_name()
    );

    let (count, template) = match resolve_interaction_response(&offer, P0, &submission(answerable))
    {
        Ok(GameAction::DeclareShortcut {
            count,
            template: Some(template),
        }) => (count, template),
        other => panic!(
            "a response pinning no \"may\" mints a declaration; got {:?}",
            other.map_err(|error| error.code)
        ),
    };
    for recorded in &recorded_mays {
        assert!(
            template.decisions.contains(recorded),
            "the declaration carries the confirmed period's own \"may\" answers"
        );
    }
    let tokens = token_count(drive.state());
    drive.take_declared(count, Some(template));
    assert!(
        matches!(drive.state().waiting_for, WaitingFor::Priority { .. }),
        "the declaration is taken and ends at priority; got {}",
        drive.state().waiting_for.variant_name()
    );
    assert!(
        token_count(drive.state()) > tokens,
        "the take replayed the period"
    );
}

/// CR 732.2a + CR 603.5: Board B's recorded offer states each "may" as fixed at the interaction
/// ingress.
#[test]
fn a_recorded_offers_may_is_fixed_at_the_interaction_ingress() {
    let Some((drive, _)) = offered(Board::B) else {
        return;
    };
    assert_recorded_mays_are_fixed_at_the_interaction_ingress(drive);
}

/// Whether `slot` names a choice of Soul's Attendant.
fn asked_of_attendant(state: &GameState, slot: &DecisionSlot) -> bool {
    match &slot.source {
        YieldTarget::ThisObject { source_id, .. } => state
            .objects
            .get(source_id)
            .is_some_and(|object| object.name == "Soul's Attendant"),
        YieldTarget::AllCopies { .. } => false,
    }
}

/// `state` is P0's priority with the tokens and the life `offer` stood at.
fn assert_nothing_was_taken(offer: &GameState, state: &GameState, row: &str) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "{row}: handed back to manual play; got {}",
        state.waiting_for.variant_name()
    );
    assert_eq!(token_count(state), token_count(offer), "{row}: tokens");
    assert_eq!(state.players[0].life, offer.players[0].life, "{row}: life");
}

/// `proposed` after every responder accepts.
fn accepted(proposed: GameState) -> GameState {
    let mut runner = GameRunner::from_state(proposed);
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("an accept is a legal response");
    }
    runner.state().clone()
}

/// CR 732.2c: a proposal restored from a save is taken only while its declared choices are the
/// ones its recorded period performs. One whose template answers a recorded "may" the other way is
/// handed back with nothing taken, through the persisted codec and through bare serde; the
/// proposal as declared is taken through both, Preston, the Vanisher making one token a
/// repetition and Soul's Attendant's taken "you may gain 1 life" gaining 1.
#[test]
fn a_restored_proposal_is_taken_only_on_its_periods_recorded_answers() {
    use crate::period_confirm_rows::reloaded;
    const COUNT: u32 = 3;
    for (board, life_gained) in [(Board::B, 0), (Board::BAttendant, COUNT as i32)] {
        let Some((drive, _)) = offered(board) else {
            return;
        };
        let offer = drive.state().clone();
        let recorded = recorded_declaration(&offer, |_| false);
        let flipped = recorded_declaration(&offer, |slot| {
            slot.point == ChoicePoint::MayGate
                && (matches!(board, Board::B) || asked_of_attendant(&offer, slot))
        });
        assert_ne!(
            flipped, recorded,
            "{board:?}: reach guard: a recorded \"may\" is answered the other way"
        );

        let mut runner = GameRunner::from_state(offer.clone());
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(COUNT),
                template: Some(recorded),
            })
            .expect("the recorded declaration is admitted");
        let matching = runner.state().clone();
        let mut mismatching = matching.clone();
        let WaitingFor::RespondToShortcut { proposal, .. } = &mut mismatching.waiting_for else {
            panic!(
                "{board:?}: reach guard: the declaration opens the response window; got {}",
                matching.waiting_for.variant_name()
            );
        };
        proposal.template = Some(flipped);

        for restored in reloaded(&matching) {
            let taken = accepted(restored);
            assert!(
                matches!(taken.waiting_for, WaitingFor::Priority { .. }),
                "{board:?}: the take ends at priority; got {}",
                taken.waiting_for.variant_name()
            );
            assert_eq!(
                token_count(&taken),
                token_count(&offer) + COUNT as usize,
                "{board:?}: tokens"
            );
            assert_eq!(
                taken.players[0].life,
                offer.players[0].life + life_gained,
                "{board:?}: life"
            );
        }
        for restored in reloaded(&mismatching) {
            assert_nothing_was_taken(&offer, &accepted(restored), &format!("{board:?}"));
        }
    }
}

/// The two answers a period asking Soul's Attendant twice records, on each board that asks it so.
const ATTENDANT_ANSWERS: [[MayChoiceOption; 2]; 2] = [
    [MayChoiceOption::Take, MayChoiceOption::Decline],
    [MayChoiceOption::Take, MayChoiceOption::Take],
];

/// The answer the standing offer's period recorded at each occurrence of Soul's Attendant's "may".
fn attendant_answers(offer: &GameState) -> Vec<(u8, MayChoiceOption)> {
    let WaitingFor::LoopShortcut { period, .. } = &offer.waiting_for else {
        panic!("an offer stands");
    };
    period
        .choices()
        .iter()
        .filter_map(|pin| match pin {
            PinnedDecision::MayChoice { slot, take } if asked_of_attendant(offer, slot) => {
                Some((slot.index, *take))
            }
            _ => None,
        })
        .collect()
}

/// Board B with Soul's Attendant, the nontoken Felidar Guardian's enters trigger aimed at Preston,
/// the Vanisher and Soul's Attendant's prompts answered `answers` in turn, driven to its offer: the
/// recorded period asks Soul's Attendant twice, and holds `answers` at its two occurrences.
fn offered_asking_attendant_twice(answers: [MayChoiceOption; 2]) -> Option<Drive> {
    let mut drive = start(Board::BAttendant)?;
    let named = |state: &GameState, id: &ObjectId, name: &str| {
        state
            .objects
            .get(id)
            .is_some_and(|object| object.name == name && !object.is_token)
    };
    let mut board_policy = std::mem::replace(&mut drive.policy, Box::new(|_| None));
    let mut asked = 0;
    drive.policy = Box::new(move |state| match &state.waiting_for {
        WaitingFor::OptionalEffectChoice { source_id, .. }
            if named(state, source_id, "Soul's Attendant") =>
        {
            let accept = answers[asked % answers.len()] == MayChoiceOption::Take;
            asked += 1;
            Some(GameAction::DecideOptionalEffect { accept })
        }
        WaitingFor::TriggerTargetSelection {
            source_id: Some(source),
            ..
        } if named(state, source, "Felidar Guardian") => engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| {
                matches!(action, GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                } if named(state, id, "Preston, the Vanisher"))
            })
            .or_else(|| board_policy(state)),
        _ => board_policy(state),
    });
    while !is_offer(drive.state()) {
        drive.drive_to_window();
    }
    assert_eq!(
        attendant_answers(drive.state()),
        [(0, answers[0]), (1, answers[1])],
        "reach guard: the period asks Soul's Attendant twice, each its own occurrence"
    );
    Some(drive)
}

/// The standing offer's recorded declaration with Soul's Attendant's two occurrences answered
/// `answers`.
fn declaring_attendant(offer: &GameState, answers: [MayChoiceOption; 2]) -> DecisionTemplate {
    let mut template = recorded_declaration(offer, |_| false);
    for pin in &mut template.decisions {
        if let PinnedDecision::MayChoice { slot, take } = pin {
            if asked_of_attendant(offer, slot) {
                *take = answers[usize::from(slot.index)];
            }
        }
    }
    template
}

/// CR 732.2a + CR 603.5: Soul's Attendant ("Whenever another creature enters, you may gain 1
/// life.") asked twice in a period is two choices, and a declaration names each: one answering
/// either occurrence otherwise than the period did is handed back with nothing taken, and the
/// period's own answers are taken, gaining 1 life for each recorded take in each repetition.
#[test]
fn a_repeated_may_is_declared_per_occurrence() {
    use MayChoiceOption::{Decline, Take};
    const COUNT: u32 = 3;
    for recorded in ATTENDANT_ANSWERS {
        let Some(mut drive) = offered_asking_attendant_twice(recorded) else {
            return;
        };
        let offer = drive.state().clone();
        for declared in [[Take, Take], [Take, Decline], [Decline, Decline]] {
            if declared == recorded {
                continue;
            }
            let mut refused = GameRunner::from_state(offer.clone());
            refused
                .act(GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(COUNT),
                    template: Some(declaring_attendant(&offer, declared)),
                })
                .expect("a refused declaration hands back rather than erroring");
            assert_nothing_was_taken(
                &offer,
                refused.state(),
                &format!("recorded {recorded:?}, declared {declared:?}"),
            );
        }

        drive.take_declared(
            IterationCount::Fixed(COUNT),
            Some(declaring_attendant(&offer, recorded)),
        );
        let taken = drive.state();
        assert!(
            matches!(taken.waiting_for, WaitingFor::Priority { .. }),
            "{recorded:?}: the take ends at priority; got {}",
            taken.waiting_for.variant_name()
        );
        assert_eq!(
            token_count(taken),
            token_count(&offer) + COUNT as usize,
            "{recorded:?}: tokens"
        );
        let takes = recorded.iter().filter(|answer| **answer == Take).count();
        assert_eq!(
            taken.players[0].life,
            offer.players[0].life + (COUNT as usize * takes) as i32,
            "{recorded:?}: life"
        );
    }
}

/// CR 732.2a + CR 603.5: a recorded offer whose period asks Soul's Attendant twice is published
/// with each occurrence as its own fixed "may", and the response declares both.
#[test]
fn a_repeated_recorded_may_is_fixed_per_occurrence_at_the_interaction_ingress() {
    for recorded in ATTENDANT_ANSWERS {
        let Some(drive) = offered_asking_attendant_twice(recorded) else {
            return;
        };
        assert_recorded_mays_are_fixed_at_the_interaction_ingress(drive);
    }
}

/// CR 732.2a: two published points on one slot name no single choice, so an offer restored with
/// such points is published to its proposer without a shortcut schema or any choice.
#[test]
fn a_restored_offer_whose_points_share_a_slot_is_not_published() {
    use crate::period_confirm_rows::reloaded;
    use engine::types::interaction::InteractionOpportunityResponse;

    for recorded in ATTENDANT_ANSWERS {
        let Some(drive) = offered_asking_attendant_twice(recorded) else {
            return;
        };
        let offer = drive.state().clone();
        let mut sharing = offer.clone();
        let WaitingFor::LoopShortcut { schema, .. } = &mut sharing.waiting_for else {
            unreachable!("the drive stands on its offer");
        };
        assert!(
            schema
                .points
                .iter()
                .any(|point| point.slot.index > 0 && asked_of_attendant(&offer, &point.slot)),
            "{recorded:?}: reach guard: a published point of Soul's Attendant is a later occurrence"
        );
        for point in &mut schema.points {
            point.slot.index = 0;
        }

        for restored in reloaded(&offer) {
            assert!(
                shortcut_schema(&published_to_p0(&restored).1).is_some(),
                "{recorded:?}: reach guard: the offer as minted is published as a shortcut schema"
            );
        }
        for restored in reloaded(&sharing) {
            let (_, view) = published_to_p0(&restored);
            assert!(
                view.opportunities.iter().all(|opportunity| matches!(
                    &opportunity.response,
                    InteractionOpportunityResponse::ExactChoices { choices } if choices.is_empty()
                )),
                "{recorded:?}: no schema and no choice is published; got {:?}",
                view.opportunities
            );
        }
    }
}

//! CR 119.3 + CR 732.2a: `DerivedViews::life_magnitude` — the engine-owned life-readout magnitude.
//! A seat reads `Unbounded` while some controller's mark records its life rising, and only the
//! crowns and the forced drain write such a mark, each ending the game; a drained seat keeps its
//! finite total. An accepted object-growth take lands its life at the take (CR 732.2c) and marks
//! no `Life` axis.

use std::collections::{BTreeMap, BTreeSet};

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{CountedTake, LoopCertificate, OfferRoad, ShortcutResponse};
use engine::analysis::resource::{ResourceAxis, ResourceVector};
use engine::game::derived_views::{derive_views, DisplayMagnitude, UnboundedResourceView};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, LoopDetectionMode, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::life_total_saturation::{life_changes, BLESSED_WINE};
use crate::loop_period_remade_token_source::{markers, repetitions};
use crate::loop_shortcut::{
    declare_and_accept, drive_collect, gunzip_dump, restore_dump, setup_2p_drain,
    setup_2p_optional_drain,
};
use crate::play_trace::{
    ability, activate, altar_board, cast, chooses_color, is_offer, names, place, settle,
};
use crate::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("seat exists")
        .life
}

/// Answers each `RespondToShortcut` with `Accept`; returns every answer's events.
fn accept_every_responder(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    while let WaitingFor::RespondToShortcut { .. } = runner.state().waiting_for {
        events.extend(
            runner
                .act(GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                })
                .expect("accept the shortcut")
                .events,
        );
    }
    events
}

/// Phyrexian Altar ("Sacrifice a creature: Add one mana of any color."), Gravecrawler ("This
/// creature can't block. You may cast this card from your graveyard as long as you control a
/// Zombie.") and Walking Corpse (a Zombie with no rules text), with Soul Warden ("Whenever another
/// creature enters, you gain 1 life.") under each named seat and P0's life at `p0_life`, driven to
/// P0's loop offer.
fn altar_offer(p0_warden: bool, p1_warden: bool, p0_life: i32) -> (GameRunner, LoopCertificate) {
    let db = shared_card_db().expect("card db");
    let (mut runner, altar, gravecrawler) = altar_board(p0_warden.then_some("Soul Warden"), db);
    if p1_warden {
        place(runner.state_mut(), P1, "Soul Warden", db);
    }
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("seat exists")
        .life = p0_life;
    let score = |a: &GameAction| 2 * names(&[gravecrawler])(a) + chooses_color(ManaType::Black)(a);
    for _ in 0..6 {
        cast(&mut runner, gravecrawler, vec![], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
        let index = ability(runner.state(), altar, true);
        activate(&mut runner, altar, index);
        settle(&mut runner, &score);
        if is_offer(runner.state()) {
            break;
        }
    }
    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the loop offer, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(proposer, P0);
    (runner, certificate)
}

fn pass_to_boundary(runner: &mut GameRunner) {
    let take_phase = runner.state().phase;
    for _ in 0..50 {
        let state = runner.state();
        if state.phase != take_phase || !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
}

fn unbounded(seats: &[PlayerId]) -> BTreeMap<PlayerId, DisplayMagnitude> {
    seats
        .iter()
        .map(|&seat| (seat, DisplayMagnitude::Unbounded))
        .collect()
}

/// CR 732.2c: the altar loop grows P0's own life (Soul Warden, CR 119.3). The accepted `Fixed(3)`
/// take lands the three gains at the take in one lump (21 → 24), marks no `Life` axis and stashes
/// nothing, so no readout claims ∞; the step boundary asks no collapse count and moves no life.
/// P1 never reads ∞.
#[test]
fn own_life_growth_reads_its_exact_total_from_the_take() {
    let (mut runner, certificate) = altar_offer(true, false, 20);
    assert_eq!(certificate.unbounded, vec![ResourceAxis::Life(P0)]);
    let offer_life = life(runner.state(), P0);
    assert_eq!(
        offer_life, 21,
        "REACH-GUARD: one Gravecrawler entered before the offer"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("declare the shortcut");
    let events = accept_every_responder(&mut runner);

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(state.phase, Phase::PreCombatMain);
    // Soul Warden gains 1 per entering creature; each of the 3 iterations casts one Gravecrawler.
    assert_eq!(life(state, P0), offer_life + 3);
    assert_eq!(
        life_changes(&events),
        vec![(P0, 3, Some(offer_life + 3))],
        "one lump at the take"
    );
    assert!(markers(&events).is_empty(), "a lump performs no repetition");
    assert!(
        state.unbounded_resources.is_empty(),
        "the take marks no axis"
    );
    assert!(
        state.pending_unbounded_materialization.is_empty(),
        "nothing waits for the boundary"
    );
    assert!(derive_views(state, None).life_magnitude.is_empty());

    pass_to_boundary(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the boundary asks no collapse count, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.phase, Phase::BeginCombat);
    assert_eq!(
        life(state, P0),
        offer_life + 3,
        "the boundary moves no life"
    );
    assert!(derive_views(state, None).life_magnitude.is_empty());
}

/// Enduring Tenacity ("Whenever you gain life, target opponent loses that much life.") and
/// Exquisite Blood ("Whenever an opponent loses life, you gain that much life."): one shortcut
/// under P0 marks both P0's gain and P3's drain, so only P0's readout is ∞.
#[test]
fn drain_victim_keeps_a_finite_readout_under_a_mixed_mark() {
    let state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    )));
    let WaitingFor::LoopShortcut {
        proposer,
        ref schema,
        ref certificate,
        ..
    } = state.waiting_for
    else {
        panic!("expected the restored offer, got {:?}", state.waiting_for);
    };
    assert_eq!(proposer, P0);
    assert_eq!(schema.iteration_count, IterationCount::UntilLethal);
    assert!(schema.points.is_empty());
    assert_eq!(
        certificate.unbounded,
        vec![ResourceAxis::Life(P0), ResourceAxis::Life(P3)]
    );

    let mut runner = GameRunner::from_state(state);
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("declare the shortcut");
    accept_every_responder(&mut runner);

    let state = runner.state();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&BTreeSet::from([
            ResourceAxis::Life(P0),
            ResourceAxis::Life(P3)
        ]))
    );
    let views = derive_views(state, None);
    assert!(views.unbounded_resources.contains(&UnboundedResourceView {
        player: P3,
        axis: ResourceAxis::Life(P3),
    }));
    assert_eq!(
        views.life_magnitude,
        BTreeMap::from([(P0, DisplayMagnitude::Unbounded)])
    );
}

/// Soul Warden's controller ("you", CR 109.5 + CR 603.3a) gains on every Gravecrawler P0's loop
/// recasts, so the offer names each Warden controller's life axis as rising. The accepted
/// `Fixed(3)` take lands each controller's three gains exactly at the take (CR 732.2c), marks and
/// stashes nothing, and no readout claims ∞, at the take or across the step boundary, which asks
/// no collapse count and moves no life.
fn warden_gains_land_exactly_at_the_take(p0_warden: bool, gaining: &[PlayerId]) {
    let (mut runner, certificate) = altar_offer(p0_warden, true, 20);
    let axes: Vec<ResourceAxis> = gaining.iter().map(|&p| ResourceAxis::Life(p)).collect();
    assert_eq!(certificate.unbounded, axes);
    assert_eq!(certificate.rising, axes);
    let offer_lives = [P0, P1].map(|p| life(runner.state(), p));

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("declare the shortcut");
    let events = accept_every_responder(&mut runner);

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(state.phase, Phase::PreCombatMain);
    for (seat, offer_life) in [P0, P1].into_iter().zip(offer_lives) {
        let gained = if gaining.contains(&seat) { 3 } else { 0 };
        assert_eq!(life(state, seat), offer_life + gained, "{seat:?}");
    }
    assert_eq!(
        life_changes(&events)
            .into_iter()
            .map(|(player, amount, _)| (player, amount))
            .collect::<Vec<_>>(),
        gaining.iter().map(|&seat| (seat, 3)).collect::<Vec<_>>(),
        "one lump per gaining seat at the take"
    );
    assert!(
        state.unbounded_resources.is_empty(),
        "the take marks no axis"
    );
    assert!(state.unbounded_rising.is_empty());
    assert!(
        state.pending_unbounded_materialization.is_empty(),
        "nothing waits for the boundary"
    );
    assert!(derive_views(state, None).life_magnitude.is_empty());
    let take_lives = [P0, P1].map(|p| life(state, p));

    pass_to_boundary(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the boundary asks no collapse count, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.phase, Phase::BeginCombat);
    assert_eq!(
        [P0, P1].map(|p| life(state, p)),
        take_lives,
        "the boundary moves no life"
    );
    assert!(derive_views(state, None).life_magnitude.is_empty());
}

/// P1's Soul Warden: "you" is P1, so P1's life at the offer plus 3 lands at the take (21 → 24 from
/// a start of 20), and P0's is unchanged.
#[test]
fn an_opponents_warden_gain_lands_exactly_at_the_loop_controllers_take() {
    warden_gains_land_exactly_at_the_take(false, &[P1]);
}

#[test]
fn both_wardens_gains_land_exactly_in_one_take() {
    warden_gains_land_exactly_at_the_take(true, &[P0, P1]);
}

/// CR 732.2a + CR 732.2b + CR 732.2c: P0's altar offer with Soul Warden is minted from P0's
/// recorded period with a counted take that performs, so the take carries out exactly the count
/// the offer admits (a count above the offer's capacity is refused at the declaration). From
/// `i32::MAX - 6` (`i32::MAX - 5` at the offer), `Fixed(5)` reaches the cap exactly and lands in
/// one lump at the take; `Fixed(6)` would pass it, so the take performs the six cycles, its
/// record keeping the first and last (CR 732.1b), and the gain the cap holds back still counts
/// (CR 119.3). From 20, P1 shortening `Fixed(3)` to 2 performs two cycles, and P1, who named that
/// place, holds priority there; `Fixed(0)` does nothing. No row marks, stashes or reads ∞, and
/// the step boundary asks no collapse count and moves no life.
#[test]
fn a_performable_life_shortcut_offer_is_carried_out_exactly_at_every_count_it_admits() {
    struct Row {
        name: &'static str,
        start: i32,
        count: u32,
        /// P1's answer: `None` accepts, `Some(n)` shortens the proposal to `n` cycles.
        shorten_to: Option<u32>,
        /// The record's repetition markers, P0's.
        kept: &'static [u32],
        /// Every `LifeChanged` the take reports, in order.
        record: Vec<(PlayerId, i32, Option<i32>)>,
        life: i32,
        gained: u32,
        priority: PlayerId,
    }
    let max = i32::MAX;
    let rows = [
        Row {
            name: "a lump reaching the cap exactly",
            start: max - 6,
            count: 5,
            shorten_to: None,
            kept: &[],
            record: vec![(P0, 5, Some(max))],
            life: max,
            gained: 6,
            priority: P0,
        },
        Row {
            name: "a take passing the cap is performed",
            start: max - 6,
            count: 6,
            shorten_to: None,
            kept: &[1, 6],
            record: vec![(P0, 1, Some(max - 4)), (P0, 1, Some(max))],
            life: max,
            gained: 7,
            priority: P0,
        },
        Row {
            name: "shortened to two cycles",
            start: 20,
            count: 3,
            shorten_to: Some(2),
            kept: &[1, 2],
            record: vec![(P0, 1, Some(22)), (P0, 1, Some(23))],
            life: 23,
            gained: 3,
            priority: P1,
        },
        Row {
            name: "zero cycles",
            start: 20,
            count: 0,
            shorten_to: None,
            kept: &[],
            record: Vec::new(),
            life: 21,
            gained: 1,
            priority: P0,
        },
    ];
    for row in rows {
        let name = row.name;
        let (mut runner, certificate) = altar_offer(true, false, row.start);
        assert_eq!(
            certificate.unbounded,
            vec![ResourceAxis::Life(P0)],
            "{name}"
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::LoopShortcut {
                    road: OfferRoad::RecordedPeriod,
                    counted_take: CountedTake::Performs,
                    ..
                }
            ),
            "{name}: a recorded period whose counted take performs, got {:?}",
            runner.state().waiting_for
        );

        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(row.count),
                template: None,
            })
            .expect("the declaration is admitted");
        let events = match row.shorten_to {
            None => accept_every_responder(&mut runner),
            Some(at_iteration) => {
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::RespondToShortcut { player, .. } if player == P1
                    ),
                    "{name}: P1 answers the proposal, got {:?}",
                    runner.state().waiting_for
                );
                runner
                    .act(GameAction::RespondToShortcut {
                        response: ShortcutResponse::Shorten { at_iteration },
                    })
                    .expect("the shortening is admitted")
                    .events
            }
        };

        let state = runner.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { player } if player == row.priority),
            "{name}: priority at the ending point, got {:?}",
            state.waiting_for
        );
        assert!(state.stack.is_empty(), "{name}: the stack is empty");
        assert_eq!(
            markers(&events),
            row.kept
                .iter()
                .map(|&repetition| (P0, repetition))
                .collect::<Vec<_>>(),
            "{name}: the record's repetitions"
        );
        assert_eq!(life_changes(&events), row.record, "{name}: the record");
        if !row.kept.is_empty() {
            assert_eq!(
                repetitions(&events)
                    .into_iter()
                    .map(life_changes)
                    .collect::<Vec<_>>(),
                row.record
                    .iter()
                    .map(|&change| vec![change])
                    .collect::<Vec<_>>(),
                "{name}: each kept cycle holds its gain"
            );
        }
        assert_eq!(life(state, P0), row.life, "{name}: P0's life");
        assert_eq!(
            state.players[0].life_gained_this_turn, row.gained,
            "{name}: P0's gained counter"
        );
        assert!(
            state.unbounded_resources.is_empty(),
            "{name}: the take marks no axis"
        );
        assert!(
            state.pending_unbounded_materialization.is_empty(),
            "{name}: nothing waits for the boundary"
        );
        assert!(
            derive_views(state, None).life_magnitude.is_empty(),
            "{name}: no readout claims ∞"
        );

        pass_to_boundary(&mut runner);
        let state = runner.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "{name}: the boundary asks no collapse count, got {:?}",
            state.waiting_for
        );
        assert_eq!(state.phase, Phase::BeginCombat, "{name}");
        assert_eq!(
            life(state, P0),
            row.life,
            "{name}: the boundary moves no life"
        );
    }
}

/// A certificate saved before `rising` existed loads with an empty subset, so the take marks no
/// seat as rising and no readout claims ∞.
#[test]
fn a_pre_field_certificate_marks_no_seat_as_rising() {
    let mut dump: serde_json::Value = serde_json::from_str(&gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    )))
    .expect("dump parses");
    let removed = dump["gameState"]["waiting_for"]["data"]["certificate"]
        .as_object_mut()
        .expect("the dump stores the offer's certificate")
        .remove("rising");
    assert_eq!(removed, Some(serde_json::json!([{ "Life": 0 }])));

    let mut runner = GameRunner::from_state(restore_dump(&dump.to_string()));
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("declare the shortcut");
    accept_every_responder(&mut runner);

    let state = runner.state();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&BTreeSet::from([
            ResourceAxis::Life(P0),
            ResourceAxis::Life(P3)
        ]))
    );
    let views = derive_views(state, None);
    for seat in [P0, P3] {
        assert!(views.unbounded_resources.contains(&UnboundedResourceView {
            player: seat,
            axis: ResourceAxis::Life(seat),
        }));
    }
    assert!(state.unbounded_rising.is_empty());
    assert!(views.life_magnitude.is_empty());
}

/// A synthetic drain, not a printed board ("Whenever you gain life, each opponent loses 1 life."
/// and "Whenever an opponent loses life, you gain 1 life."): P0's life rises and P1's falls
/// (CR 704.5a), so on every crowning producer only P0 reads ∞.
#[test]
fn crowned_drains_read_only_the_gaining_seat_as_unbounded() {
    type Crown = fn() -> GameRunner;
    let crowns: [(&str, Crown); 3] = [
        ("On", || {
            let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::On);
            let _ = runner.cast(kickoff).resolve();
            let _ = drive_collect(&mut runner, 500);
            runner
        }),
        ("mandatory Interactive", || {
            let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::Interactive);
            let _ = runner.cast(kickoff).resolve();
            let _ = drive_collect(&mut runner, 500);
            runner
        }),
        ("UntilLethal", || {
            let (mut runner, kickoff, _bolt, _cleric) =
                setup_2p_optional_drain(LoopDetectionMode::Interactive);
            let _ = runner.cast(kickoff).resolve();
            let (_events, offer) = drive_collect(&mut runner, 2000);
            assert!(matches!(offer, WaitingFor::LoopShortcut { .. }));
            declare_and_accept(&mut runner, IterationCount::UntilLethal);
            runner
        }),
    ];
    for (route, crown) in crowns {
        let runner = crown();
        let state = runner.state();
        assert_eq!(
            state.waiting_for,
            WaitingFor::GameOver { winner: Some(P0) },
            "{route}"
        );
        let marked = state
            .unbounded_resources
            .get(&P0)
            .cloned()
            .unwrap_or_default();
        assert!(
            marked.contains(&ResourceAxis::Life(P0)) && marked.contains(&ResourceAxis::Life(P1)),
            "{route}: {marked:?}"
        );
        assert_eq!(
            state.unbounded_rising,
            BTreeMap::from([(P0, BTreeSet::from([ResourceAxis::Life(P0)]))]),
            "{route}"
        );
        assert_eq!(
            derive_views(state, None).life_magnitude,
            unbounded(&[P0]),
            "{route}"
        );
    }
}

/// Marauding Blight-Priest ("Whenever you gain life, each opponent loses 1 life.") and Exquisite
/// Blood ("Whenever an opponent loses life, you gain that much life."), under P0, started by
/// Blessed Wine ("You gain 1 life." / "Draw a card at the beginning of the next turn's
/// upkeep."): each gain drains P1 1 and each loss gains P0 that 1, a loop of mandatory triggers
/// in which P0's life rises and P1's falls (CR 704.5a). The detector (`LoopDetectionMode::On`)
/// takes it as a shortcut (CR 732.2a) and ends the game with P0 the winner, marking both seats'
/// life and recording only P0's as rising, so only P0 reads ∞. That game-over frame emits the
/// client's life wire golden (`client/src/test/fixtures/unbounded-life-wire.json`); regenerate it
/// with `UPDATE_WIRE_GOLDEN=1` and this test's name as the filter.
#[test]
fn blight_priest_and_exquisite_blood_crown_reads_only_the_gaining_seat_as_unbounded() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_real_card(P0, "Marauding Blight-Priest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::On;

    let mut events = runner.cast(kickoff).resolve().events().to_vec();
    events.extend(drive_collect(&mut runner, 500).0);

    assert_eq!(
        life_changes(&events).get(..3),
        Some(&[(P0, 1, Some(21)), (P1, -1, Some(5)), (P0, 1, Some(22))][..]),
        "REACH-GUARD: Blessed Wine's gain, the Blight-Priest's drain and Exquisite Blood's gain \
         of that much"
    );
    let state = runner.state();
    assert_eq!(state.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
    let marked = state
        .unbounded_resources
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        marked.contains(&ResourceAxis::Life(P0)) && marked.contains(&ResourceAxis::Life(P1)),
        "{marked:?}"
    );
    assert_eq!(
        state.unbounded_rising,
        BTreeMap::from([(P0, BTreeSet::from([ResourceAxis::Life(P0)]))])
    );
    let views = derive_views(state, None);
    assert_eq!(views.life_magnitude, unbounded(&[P0]));

    let wire = serde_json::to_value(&views).expect("derived views serialize");
    let golden = serde_json::json!({
        "life_magnitude": wire
            .get("life_magnitude")
            .expect("the game-over frame emits life_magnitude")
            .clone(),
    });
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../client/src/test/fixtures/unbounded-life-wire.json"
    );
    if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&golden).unwrap()),
        )
        .expect("write the wire golden");
    }
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("committed wire golden"))
            .unwrap();
    assert_eq!(
        golden, committed,
        "the client's life wire golden drifted from engine output — re-run with \
         UPDATE_WIRE_GOLDEN=1"
    );
}

/// CR 732.2a: the rising subset is `unbounded_axes_for` minus every axis the delta drives down.
#[test]
fn rising_axes_are_the_unbounded_axes_the_delta_drives_up() {
    let delta = ResourceVector {
        life: BTreeMap::from([(P0, 1), (P1, 1), (P2, -1)]),
        library_delta: BTreeMap::from([(P1, -2), (P2, 3)]),
        ..Default::default()
    };
    let all: BTreeSet<ResourceAxis> = delta.unbounded_axes_for(P0).into_iter().collect();
    let rising: BTreeSet<ResourceAxis> = delta.rising_unbounded_axes_for(P0).into_iter().collect();
    assert_eq!(
        all,
        BTreeSet::from([
            ResourceAxis::Life(P0),
            ResourceAxis::Life(P1),
            ResourceAxis::Life(P2),
            ResourceAxis::LibraryDelta(P1),
            ResourceAxis::LibraryDelta(P2),
        ])
    );
    assert_eq!(
        rising,
        BTreeSet::from([
            ResourceAxis::Life(P0),
            ResourceAxis::Life(P1),
            ResourceAxis::LibraryDelta(P2),
        ])
    );
    assert_eq!(
        all.difference(&rising).copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([ResourceAxis::Life(P2), ResourceAxis::LibraryDelta(P1)])
    );

    let own_loss = ResourceVector {
        life: BTreeMap::from([(P0, -1), (P1, 1)]),
        ..Default::default()
    };
    assert_eq!(
        own_loss.unbounded_axes_for(P0),
        vec![ResourceAxis::Life(P1)]
    );
    assert_eq!(
        own_loss.rising_unbounded_axes_for(P0),
        vec![ResourceAxis::Life(P1)]
    );
}

/// CR 119.3 + CR 732.2a: a seat reads ∞ iff some controller's mark records its life as rising.
#[test]
fn a_seat_reads_unbounded_iff_some_mark_records_its_life_rising() {
    let [l0, l1] = [P0, P1].map(ResourceAxis::Life);
    let fresh = || GameState::new(FormatConfig::standard(), 3, 42);

    let mut state = fresh();
    state.mark_unbounded_loop(P0, &[l0, l1], &[l0, l1]);
    assert_eq!(
        derive_views(&state, None).life_magnitude,
        unbounded(&[P0, P1])
    );

    let mut state = fresh();
    state.mark_unbounded_loop(P0, &[l0, l1], &[l0]);
    assert_eq!(derive_views(&state, None).life_magnitude, unbounded(&[P0]));

    let mut state = fresh();
    state.mark_unbounded_loop(P0, &[l1], &[]);
    state.mark_unbounded_loop(P2, &[l1], &[l1]);
    assert_eq!(derive_views(&state, None).life_magnitude, unbounded(&[P1]));

    let mut state = fresh();
    state.mark_unbounded_loop(P0, &[l1], &[]);
    state.mark_unbounded_loop(P2, &[l1], &[]);
    assert!(derive_views(&state, None).life_magnitude.is_empty());
}

/// A mark saved before `unbounded_rising` existed loads with no rising entry, so its badge row
/// stays and no readout claims ∞.
#[test]
fn a_pre_field_mark_keeps_its_badge_and_claims_no_unbounded_life() {
    let l0 = ResourceAxis::Life(P0);
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    state.mark_unbounded_loop(P0, &[l0], &[l0]);
    let saved = serde_json::to_value(&state).expect("state serializes");

    let restored: GameState = serde_json::from_value(saved.clone()).expect("state deserializes");
    assert_eq!(
        derive_views(&restored, None).life_magnitude,
        unbounded(&[P0])
    );

    let mut stripped = saved;
    assert!(stripped
        .as_object_mut()
        .expect("state serializes as an object")
        .remove("unbounded_rising")
        .is_some());
    let restored: GameState = serde_json::from_value(stripped).expect("state deserializes");
    let views = derive_views(&restored, None);
    assert!(views.unbounded_resources.contains(&UnboundedResourceView {
        player: P0,
        axis: l0,
    }));
    assert!(views.life_magnitude.is_empty());
}

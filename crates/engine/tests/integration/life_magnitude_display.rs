//! CR 119.3 + CR 732.2a: `DerivedViews::life_magnitude` — the engine-owned life-readout magnitude.
//! A seat reads `Unbounded` while an accepted shortcut marks its life as rising, under any
//! controller; a drained seat keeps its finite total.

use std::collections::{BTreeMap, BTreeSet};

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{LoopCertificate, ShortcutResponse};
use engine::analysis::resource::{ResourceAxis, ResourceVector};
use engine::game::derived_views::{derive_views, DisplayMagnitude, UnboundedResourceView};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, PersistentAxisMaterialization, WaitingFor,
};
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::loop_shortcut::{
    declare_and_accept, drive_collect, gunzip_dump, restore_dump, setup_2p_drain,
    setup_2p_optional_drain,
};
use crate::play_trace::{
    ability, activate, altar_board_with, cast, chooses_color, is_offer, names, place, settle,
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

fn accept_every_responder(runner: &mut GameRunner) {
    while let WaitingFor::RespondToShortcut { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accept the shortcut");
    }
}

/// Phyrexian Altar ("Sacrifice a creature: Add one mana of any color."), Gravecrawler ("This
/// creature can't block. You may cast this card from your graveyard as long as you control a
/// Zombie.") and Walking Corpse (a Zombie with no rules text), with Soul Warden ("Whenever another
/// creature enters, you gain 1 life.") under each named seat, driven to P0's loop offer.
fn altar_offer(p0_warden: bool, p1_warden: bool) -> (GameRunner, LoopCertificate) {
    altar_offer_with(p0_warden, p1_warden, |_| {})
}

fn altar_offer_with(
    p0_warden: bool,
    p1_warden: bool,
    customize: impl FnOnce(&mut GameScenario),
) -> (GameRunner, LoopCertificate) {
    let db = shared_card_db().expect("card db");
    let (mut runner, altar, gravecrawler) =
        altar_board_with(p0_warden.then_some("Soul Warden"), db, customize);
    if p1_warden {
        place(runner.state_mut(), P1, "Soul Warden", db);
    }
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

/// The altar loop grows P0's own life, so P0 reads ∞ from the take until the collapse lands, and
/// P1 never does.
#[test]
fn own_life_growth_reads_unbounded_until_the_collapse_lands() {
    let (mut runner, certificate) = altar_offer(true, false);
    assert_eq!(certificate.unbounded, vec![ResourceAxis::Life(P0)]);
    let offer_life = life(runner.state(), P0);

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("declare the shortcut");
    accept_every_responder(&mut runner);

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.phase, Phase::PreCombatMain);
    assert_eq!(
        state.unbounded_resources,
        BTreeMap::from([(P0, BTreeSet::from([ResourceAxis::Life(P0)]))])
    );
    let views = derive_views(state, None);
    let wire = serde_json::to_value(&views).expect("derived views serialize");
    let golden = serde_json::json!({
        "life_magnitude": wire
            .get("life_magnitude")
            .expect("the take frame emits life_magnitude")
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
    let unbounded_p0 = BTreeMap::from([(P0, DisplayMagnitude::Unbounded)]);
    assert_eq!(views.life_magnitude, unbounded_p0);

    pass_to_boundary(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::PayAmountChoice { player, .. } if player == P0),
        "the collapse prompts P0 at the boundary, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.phase, Phase::BeginCombat);
    assert_eq!(derive_views(state, None).life_magnitude, unbounded_p0);

    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("submit the collapse count");
    let state = runner.state();
    // Soul Warden gains 1 per entering creature; each of the 3 iterations casts one Gravecrawler.
    assert_eq!(life(state, P0), offer_life + 3);
    assert!(state.unbounded_resources.is_empty());
    assert!(derive_views(state, None).life_magnitude.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));

    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("committed wire golden"))
            .unwrap();
    assert_eq!(
        golden, committed,
        "the client's life wire golden drifted from engine output — re-run with \
         UPDATE_WIRE_GOLDEN=1"
    );
}

/// CR 732.2a + CR 119.3: An accepted marked shortcut records the agreed count, but an intervening
/// ordinary action taken during priority before the phase boundary (e.g. Chaplain's Blessing gaining
/// 5 life) moves life closer to the limit. The boundary choice prompt re-validates the live
/// representable headroom without exceeding the count the players agreed to (#10543).
#[test]
fn marked_shortcut_revalidates_representable_bound_at_boundary_after_intervening_gain() {
    let (mut runner, certificate) = altar_offer_with(true, false, |s| {
        s.add_spell_to_hand_from_oracle(P0, "Chaplain's Blessing", false, "You gain 5 life.")
            .with_mana_cost(ManaCost::zero());
    });
    assert_eq!(certificate.unbounded, vec![ResourceAxis::Life(P0)]);

    // Set P0 near the limit where exactly 10 repetitions fit (P0 life = i32::MAX - 10).
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .life = i32::MAX - 10;

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(10),
            template: None,
        })
        .expect("declare 10 repetitions");
    accept_every_responder(&mut runner);

    // The accepted shortcut marks life growth and returns ordinary priority before the
    // phase-boundary choice.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        runner.state().pending_materialization_count.get(&P0),
        Some(&10)
    );

    // Intervening priority: resolve Chaplain's Blessing ("You gain 5 life.").
    let blessing = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .hand
        .iter()
        .copied()
        .find(|&id| runner.state().objects[&id].name == "Chaplain's Blessing")
        .expect("Chaplain's Blessing in hand");

    cast(&mut runner, blessing, vec![], CastPaymentMode::Auto);
    settle(&mut runner, &|_| 0);

    // P0's life is now i32::MAX - 5; only 5 repetitions can now be represented.
    assert_eq!(life(runner.state(), P0), i32::MAX - 5);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // Pass to the phase boundary. The collapse prompt must re-validate the live bound,
    // capping max at 5 rather than the previously agreed 10.
    pass_to_boundary(&mut runner);
    let state = runner.state();
    let WaitingFor::PayAmountChoice {
        player, min, max, ..
    } = state.waiting_for
    else {
        panic!(
            "expected PayAmountChoice at boundary, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(min, 0);
    assert_eq!(max, 5);

    // Attempting to submit 10 exceeds the live representable max and is rejected.
    let over = runner.act(GameAction::SubmitPayAmount { amount: 10 });
    assert!(
        over.is_err(),
        "submitting 10 when only 5 can be represented must be rejected"
    );

    // Submitting 5 succeeds and lands P0 on exactly i32::MAX without overflow.
    runner
        .act(GameAction::SubmitPayAmount { amount: 5 })
        .expect("submit live representable amount 5");
    let state = runner.state();
    assert_eq!(life(state, P0), i32::MAX);
    assert!(state.unbounded_resources.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 732.2a + CR 119.3: Multiple accepted marked shortcuts in the stash for the same player
/// each apply their per-cycle gain to the submitted amount at the boundary. The boundary prompt
/// aggregates the per-cycle gain across all pending items for that player before computing the
/// live representable bound, preventing the prompt from offering counts whose materialization
/// would overflow the stored scalar (#10543).
#[test]
fn multiple_accepted_marked_life_batches_aggregate_per_cycle_deltas_at_boundary() {
    let db = shared_card_db().expect("card db");
    let (mut runner, altar, gravecrawler) = altar_board_with(Some("Soul Warden"), db, |_| {});
    let score = |a: &GameAction| 2 * names(&[gravecrawler])(a) + chooses_color(ManaType::Black)(a);

    // Drive to the first loop shortcut offer.
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
    assert!(is_offer(runner.state()));
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(10),
            template: None,
        })
        .expect("declare first shortcut");
    accept_every_responder(&mut runner);

    // First shortcut is accepted. Verify 1 pending materialization item.
    assert_eq!(
        runner
            .state()
            .pending_unbounded_materialization
            .get(&P0)
            .map(|v| v.len()),
        Some(1)
    );

    // Priority returns to P0 before the phase boundary. Drive to a second loop shortcut offer.
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
    assert!(is_offer(runner.state()));
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(10),
            template: None,
        })
        .expect("declare second shortcut");
    accept_every_responder(&mut runner);

    // Second shortcut is accepted. P0 owns one stash holding both accepted items.
    assert_eq!(
        runner
            .state()
            .pending_unbounded_materialization
            .get(&P0)
            .map(|v| v.len()),
        Some(2)
    );

    // Set P0 near the limit where exactly 10 life of headroom remains (P0 life = i32::MAX - 10).
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .life = i32::MAX - 10;

    // Pass to the phase boundary. The collapse prompt must aggregate the 2 pending items
    // (1 + 1 = 2 life per repetition) against the 10 life of headroom, capping max at 5
    // rather than offering 10 (which would apply 10 * 2 = 20 life and overflow).
    pass_to_boundary(&mut runner);
    let state = runner.state();
    let WaitingFor::PayAmountChoice {
        player, min, max, ..
    } = state.waiting_for
    else {
        panic!(
            "expected PayAmountChoice at boundary, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(min, 0);
    assert_eq!(
        max, 5,
        "aggregated per-cycle delta (1+1=2) caps 10 headroom at 5 repetitions"
    );

    // Submitting 6 exceeds the aggregated representable max and is rejected.
    let over = runner.act(GameAction::SubmitPayAmount { amount: 6 });
    assert!(
        over.is_err(),
        "submitting 6 when only 5 can be represented must be rejected"
    );

    // Submitting 5 succeeds, applying 5 to both items (+5 + +5 = +10), landing P0 on exactly i32::MAX.
    runner
        .act(GameAction::SubmitPayAmount { amount: 5 })
        .expect("submit live representable amount 5");
    let state = runner.state();
    assert_eq!(life(state, P0), i32::MAX);
    assert!(state.unbounded_resources.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
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

/// Soul Warden's controller ("you") gains on every Gravecrawler P0's loop recasts, so each
/// Warden controller's life rises under P0's mark and reads ∞ until the collapse lands.
fn warden_gains_read_unbounded(p0_warden: bool, gaining: &[PlayerId]) {
    let (mut runner, certificate) = altar_offer(p0_warden, true);
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
    accept_every_responder(&mut runner);

    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.phase, Phase::PreCombatMain);
    let marked = BTreeMap::from([(P0, axes.iter().copied().collect::<BTreeSet<_>>())]);
    assert_eq!(state.unbounded_resources, marked);
    assert_eq!(state.unbounded_rising, marked);
    let pending = state
        .pending_unbounded_materialization
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    let views = derive_views(state, None);
    for &seat in gaining {
        assert!(views.unbounded_resources.contains(&UnboundedResourceView {
            player: seat,
            axis: ResourceAxis::Life(seat),
        }));
        assert!(pending.iter().any(
            |item| matches!(item, PersistentAxisMaterialization::Life { player, .. } if *player == seat)
        ));
    }
    assert_eq!(views.life_magnitude, unbounded(gaining));

    pass_to_boundary(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::PayAmountChoice { player, .. } if player == P0),
        "the collapse prompts P0 at the boundary, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.phase, Phase::BeginCombat);
    assert_eq!(derive_views(state, None).life_magnitude, unbounded(gaining));

    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("submit the collapse count");
    let state = runner.state();
    for (seat, offer_life) in [P0, P1].into_iter().zip(offer_lives) {
        let gained = if gaining.contains(&seat) { 3 } else { 0 };
        assert_eq!(life(state, seat), offer_life + gained, "{seat:?}");
    }
    assert!(derive_views(state, None).life_magnitude.is_empty());
    assert!(state.unbounded_rising.is_empty());
}

#[test]
fn an_opponents_warden_gain_reads_unbounded_under_the_loop_controllers_mark() {
    warden_gains_read_unbounded(false, &[P1]);
}

#[test]
fn both_wardens_gains_read_unbounded_under_one_mark() {
    warden_gains_read_unbounded(true, &[P0, P1]);
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

//! CR 601.2c + CR 601.2f + CR 602.2b: an activation whose cost may depend on
//! its targets (Professor Hojo's discount, Kopala's tax) is locked once, at
//! target settlement, with every modifier: raises first, then reductions, under
//! the caster's election when more than one total is reachable. Before targets
//! exist, affordability is judged over the legal assignments.

use engine::game::casting::can_activate_ability_now;
use engine::game::filter_state_for_viewer;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, StaticDefinition, TargetRef};
use engine::types::actions::GameAction;
use engine::types::casting_costs::{ActivationCostLock, ActivationCostLockPoint};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AbilityActivationRecord, ActivationTargetFact, CostResume, ManaChoice, PersistedGameState,
    PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ActivationExemption, CastFrequency, CostModifyMode, StaticMode};
use engine::types::zones::Zone;

const HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.\nWhenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";
const KOPALA: &str = "Spells your opponents cast that target a Merfolk you control cost {2} more to cast.\nAbilities your opponents activate that target a Merfolk you control cost {2} more to activate.";
const GROUNDS_FLOORED: &str = "Activated abilities of creatures you control cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.";
const ONE_LESS_UNFLOORED: &str =
    "Activated abilities of creatures you control cost {1} less to activate.";
const BREYA: &str = "When Breya enters, create two 1/1 blue Thopter artifact creature tokens with flying.\n{2}, Sacrifice two artifacts: Choose one \u{2014}\n\u{2022} Breya deals 3 damage to target player or planeswalker.\n\u{2022} Target creature gets -4/-4 until end of turn.\n\u{2022} You gain 5 life.";

fn pool(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn mana(s: &mut GameScenario, player: PlayerId, n: usize) {
    s.with_mana_pool(
        player,
        (0..n)
            .map(|_| ManaUnit::new(ManaColor::Blue.into(), ObjectId(0), false, Vec::new()))
            .collect(),
    );
}

fn library(s: &mut GameScenario, player: PlayerId) {
    for name in ["L1", "L2", "L3", "L4"] {
        s.add_card_to_library_top(player, name);
    }
}

fn unsick(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .has_summoning_sickness = false;
}

fn act(runner: &mut GameRunner, action: GameAction) -> Result<WaitingFor, String> {
    runner
        .act(action)
        .map(|result| result.waiting_for)
        .map_err(|error| format!("{error:?}"))
}

fn object(target: ObjectId) -> TargetRef {
    TargetRef::Object(target)
}

/// Pays whatever the activation still asks for, from the pool and the named
/// objects, until it reaches the stack. Panics on any other prompt.
fn finish(runner: &mut GameRunner, pay_with: &[ObjectId]) -> WaitingFor {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => return runner.state().waiting_for.clone(),
            WaitingFor::ManaPayment { .. } => {
                act(runner, GameAction::PassPriority).expect("mana payment");
            }
            WaitingFor::PayCost { .. } => {
                act(
                    runner,
                    GameAction::SelectCards {
                        cards: pay_with.to_vec(),
                    },
                )
                .expect("cost payment");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                act(
                    runner,
                    GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    },
                )
                .expect("trigger order");
            }
            other => panic!("unexpected prompt while paying: {other:?}"),
        }
    }
    panic!("the activation never reached the stack")
}

fn stack_len(runner: &GameRunner) -> usize {
    runner.state().stack.len()
}

fn election_totals(waiting_for: &WaitingFor) -> Vec<u32> {
    match waiting_for {
        WaitingFor::OrderCostReductions { outcomes, .. } => outcomes
            .iter()
            .map(|outcome| outcome.locked_cost.mana_value())
            .collect(),
        other => panic!("expected OrderCostReductions, got {other:?}"),
    }
}

fn elect_total(runner: &mut GameRunner, total: u32) -> Result<WaitingFor, String> {
    let WaitingFor::OrderCostReductions { outcomes, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected an election prompt");
    };
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.locked_cost.mana_value() == total)
        .unwrap_or_else(|| panic!("no outcome locks {total}"));
    act(
        runner,
        GameAction::OrderCostReductions {
            order: outcome.order.clone(),
            hybrid_announcement: Vec::new(),
        },
    )
}

/// The engine's own save/undo/P2P restore pipeline (`PersistedGameState`), not
/// a bare `serde_json::from_str::<GameState>`.
fn round_trip(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    restore_json(&json)
}

fn restore_json(json: &str) -> GameRunner {
    let state = serde_json::from_str::<PersistedGameState>(json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

// ---------------------------------------------------------------------------
// M2: every raise before every reduction, across the two kinds of modifier.
// ---------------------------------------------------------------------------

/// P0's creature ability `{2}: Tap target creature`, reduced by P0's floored
/// Training Grounds, taxed by P1's Kopala when it targets the Merfolk.
fn grounds_and_kopala_paid(target_merfolk: bool) -> usize {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_artifact_from_oracle(P0, "Training Grounds", GROUNDS_FLOORED);
    let src = s
        .add_creature_from_oracle(P0, "Tapper", 2, 2, "{2}: Tap target creature.")
        .id();
    s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
        .with_subtypes(vec!["Merfolk", "Wizard"]);
    let merfolk = s
        .add_creature(P1, "Merfolk", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let bear = s.add_creature(P1, "Bear", 2, 2).id();
    mana(&mut s, P0, 8);
    let mut r = s.build();
    unsick(&mut r, src);
    let before = pool(&r, P0);
    r.activate(src, 0)
        .target_object(if target_merfolk { merfolk } else { bear })
        .resolve();
    before - pool(&r, P0)
}

/// CR 601.2f: "plus all ... cost increases, and minus all cost reductions".
/// `{2}` + Kopala's `{2}` = `{4}`, then Grounds' `-2` (floor one) = `{2}`. The
/// old two-pass fold reduced first (`{2}` floored at `{1}`) and taxed after
/// (`{3}`).
#[test]
fn a_target_gated_raise_is_applied_before_a_floored_reduction() {
    assert_eq!(grounds_and_kopala_paid(true), 2, "raise, then reduction");
    assert_eq!(
        grounds_and_kopala_paid(false),
        1,
        "control: the untaxed target pays the floored {{1}}"
    );
}

// ---------------------------------------------------------------------------
// M1 / O1-O4: whether the activation is OFFERED, judged over the legal
// assignments, before any target exists.
// ---------------------------------------------------------------------------

fn kopala_offer(
    have: usize,
    with_bear: bool,
) -> (bool, perf_counters::ActivationCostRouteCounters) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    // Kopala is itself a Merfolk, so it is taxed as a target too.
    s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
        .with_subtypes(vec!["Merfolk", "Wizard"]);
    s.add_creature(P1, "Merfolk", 2, 2)
        .with_subtypes(vec!["Merfolk"]);
    if with_bear {
        s.add_creature(P1, "Bear", 2, 2);
    }
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{2}: Tap target creature.")
        .id();
    mana(&mut s, P0, have);
    let r = s.build();
    perf_counters::reset();
    let offered = can_activate_ability_now(r.state(), P0, src, 0);
    (offered, perf_counters::activation_cost_route_snapshot())
}

/// O1: the Merfolk is taxed to `{4}`, but the Bear is a legal target at `{2}`,
/// so exactly `{2}` makes the activation affordable. The old Feasibility pass
/// applied the raise whenever SOME assignment qualified, and hid it.
#[test]
fn an_activation_affordable_on_an_untaxed_target_is_offered() {
    let (offered, routes) = kopala_offer(2, true);
    assert!(offered, "the Bear makes it affordable at {{2}}");
    assert_eq!(
        routes.window_searches, 1,
        "reach guard: the bounds didn't decide it, so the assignment walk did"
    );
    let (offered, _) = kopala_offer(1, true);
    assert!(!offered, "control: {{1}} affords no assignment");
}

/// O2: with only the Merfolk to target, every assignment is taxed.
#[test]
fn an_activation_whose_every_assignment_is_taxed_out_of_reach_is_not_offered() {
    let (offered, routes) = kopala_offer(2, false);
    assert!(!offered, "the only legal target costs {{4}}");
    assert_eq!(
        routes.window_searches, 1,
        "reach guard: the walk decided it"
    );
    let (offered, routes) = kopala_offer(4, false);
    assert!(offered, "control: {{4}} affords the taxed target");
    assert_eq!(
        routes.window_searches, 0,
        "the worst case is payable: no walk"
    );
}

fn hojo_offer(have: usize, own_creature: bool) -> bool {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    s.add_creature(P1, "Bear", 2, 2);
    let text = if own_creature {
        "{3}: Tap target creature."
    } else {
        "{3}: Tap target creature an opponent controls."
    };
    let src = s.add_artifact_from_oracle(P0, "Tapper", text).id();
    mana(&mut s, P0, have);
    let r = s.build();
    can_activate_ability_now(r.state(), P0, src, 0)
}

/// O3 / O4: Hojo's `{2}` discount makes `{3}` affordable with `{1}` only if a
/// creature you control is a legal target.
#[test]
fn a_discount_makes_an_activation_offered_only_when_a_qualifying_target_exists() {
    assert!(hojo_offer(1, true), "O3: target Hojo itself at {{1}}");
    assert!(
        !hojo_offer(1, false),
        "O4: no creature you control can be targeted"
    );
    assert!(
        hojo_offer(3, false),
        "control: {{3}} pays the undiscounted price"
    );
}

// ---------------------------------------------------------------------------
// H1: settlement refuses an unaffordable target before any payment.
// ---------------------------------------------------------------------------

fn sacrifice_board(have: usize) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
        .with_subtypes(vec!["Merfolk", "Wizard"]);
    let merfolk = s
        .add_creature(P1, "Merfolk", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let bear = s.add_creature(P1, "Bear", 2, 2).id();
    let fodder = s.add_creature(P0, "Fodder", 1, 1).id();
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Altar",
            "{2}, Sacrifice a creature: Tap target creature.",
        )
        .id();
    mana(&mut s, P0, have);
    (s.build(), src, merfolk, bear, fodder)
}

/// The whole game state as JSON, minus the fields a reversal legitimately
/// moves: the revision (the transport's staleness key) and the interaction
/// authority bound to the current decision.
fn state_without_revision(r: &GameRunner) -> serde_json::Value {
    let mut value = serde_json::to_value(r.state()).unwrap();
    let object = value.as_object_mut().unwrap();
    for key in [
        "state_revision",
        "active_interaction_slots",
        "interaction_authority",
        "next_interaction_slot_id",
    ] {
        object.remove(key);
    }
    value
}

/// The paths at which two JSON values differ, for readable failures.
fn json_diff(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    match (a, b) {
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            for key in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                let null = serde_json::Value::Null;
                json_diff(
                    &format!("{path}.{key}"),
                    x.get(key).unwrap_or(&null),
                    y.get(key).unwrap_or(&null),
                    out,
                );
            }
        }
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                json_diff(&format!("{path}[{i}]"), p, q, out);
            }
        }
        _ if a != b => out.push(format!("{path}: {a} -> {b}")),
        _ => {}
    }
}

#[track_caller]
fn assert_same_state(before: &serde_json::Value, after: &serde_json::Value, what: &str) {
    let mut diffs = Vec::new();
    json_diff("", before, after, &mut diffs);
    assert!(diffs.is_empty(), "{what}: {diffs:#?}");
}

fn tap_land_for_mana(r: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = engine::ai_support::legal_actions_full(r.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the engine authors the land's mana selection");
    act(r, GameAction::TapLandForMana { selection }).expect("tap the land");
    assert_eq!(
        r.state().lands_tapped_for_mana.get(&P0),
        Some(&vec![land]),
        "reach guard: the tap opened a mana-undo window"
    );
}

/// CR 601.2h + CR 733.1: with exactly `{2}`, choosing the Merfolk makes the
/// total `{4}`. The activation can't be completed legally, so it is reversed
/// ENTIRELY, to priority, with the state from before `ActivateAbility`: the
/// target declaration and every other pre-lock trace are gone. CR 733.2: a
/// fresh, legal activation choosing the Bear then succeeds.
#[test]
fn an_unaffordable_target_reverses_the_whole_activation_to_priority() {
    let (mut r, src, merfolk, bear, fodder) = sacrifice_board(2);
    // Settle the scenario's lazily-initialized layer bases first, so the
    // comparison sees only what the activation did.
    engine::game::layers::flush_layers(r.state_mut());
    let before_activation = state_without_revision(&r);
    let wf = act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("offered: the Bear is affordable");
    assert!(matches!(wf, WaitingFor::TargetSelection { .. }), "{wf:?}");

    let result = r
        .act(GameAction::SelectTargets {
            targets: vec![object(merfolk)],
        })
        .expect("a reversal is a result, not a rejection");
    assert!(!result.disposition.is_applied(), "typed reversal");
    assert!(matches!(result.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_same_state(
        &before_activation,
        &state_without_revision(&r),
        "after the reversal",
    );
    assert_eq!(pool(&r, P0), 2);
    assert_eq!(r.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(stack_len(&r), 0);

    // CR 733.2: a fresh activation, choosing the untaxed Bear.
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("a fresh activation");
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(bear)],
        },
    )
    .expect("the untaxed target is affordable");
    finish(&mut r, &[fodder]);
    assert_eq!(pool(&r, P0), 0);
    assert_eq!(r.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(stack_len(&r), 1);
}

/// The same reversal keeps an accumulating loop period and a real manual
/// mana-undo window exactly as they were: nothing was accepted before the lock.
#[test]
fn an_unaffordable_target_reversal_keeps_the_loop_period_and_mana_undo_window() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
        .with_subtypes(vec!["Merfolk", "Wizard"]);
    let merfolk = s
        .add_creature(P1, "Merfolk", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    s.add_creature(P1, "Bear", 2, 2);
    s.add_creature(P0, "Fodder", 1, 1);
    let land = s.add_basic_land(P0, ManaColor::White);
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Altar",
            "{2}, Sacrifice a creature: Tap target creature. Create a 1/1 white Soldier creature token.",
        )
        .id();
    mana(&mut s, P0, 1);
    let mut r = s.build();
    r.state_mut().loop_detection = engine::types::game_state::LoopDetectionMode::On;
    let card_id = r.state().objects[&src].card_id;
    r.state_mut().last_loop_action_sequence = vec![engine::types::game_state::LoopActionContext {
        card_id,
        controller: P0,
        action: engine::types::game_state::LoopAction::Activate {
            source_id: src,
            ability_index: 0,
        },
        convoke: None,
        pins: Vec::new(),
    }];
    tap_land_for_mana(&mut r, land);
    let before = state_without_revision(&r);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("offered: the Bear is affordable");
    let result = r
        .act(GameAction::SelectTargets {
            targets: vec![object(merfolk)],
        })
        .expect("a reversal is a result");
    assert!(!result.disposition.is_applied());
    assert_same_state(&before, &state_without_revision(&r), "after the reversal");
    act(&mut r, GameAction::UntapLandForMana { object_id: land })
        .expect("the mana-undo window survived the reversal");
}

/// H1-4: a reversed activation spends nothing, including Hojo's once-per-turn
/// slot.
#[test]
fn a_reversed_activation_leaves_the_once_per_turn_discount_unspent() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    let fodder = s.add_creature(P0, "Fodder", 1, 1).id();
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Altar",
            "{4}, Sacrifice a creature: Tap target creature.",
        )
        .id();
    let bear = s.add_creature(P1, "Bear", 2, 2).id();
    mana(&mut s, P0, 2);
    let mut r = s.build();
    let activate = |r: &mut GameRunner| {
        act(
            r,
            GameAction::ActivateAbility {
                source_id: src,
                ability_index: 0,
            },
        )
        .expect("offered through Hojo's discount")
    };
    activate(&mut r);
    let result = r
        .act(GameAction::SelectTargets {
            targets: vec![object(bear)],
        })
        .expect("a reversal is a result");
    assert!(
        !result.disposition.is_applied(),
        "the Bear gets no discount, so {{4}} is unaffordable"
    );
    assert!(
        r.state().abilities_activated_this_turn_by_player.is_empty(),
        "a reversed activation leaves no journal row"
    );
    activate(&mut r);
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(own)],
        },
    )
    .expect("your own creature gets Hojo's discount");
    finish(&mut r, &[fodder]);
    assert_eq!(pool(&r, P0), 0, "paid {{2}}: the discount was still there");
}

/// HIGH 2: a deferred lock with no election accepts the activation exactly
/// once, at the lock, before payment: its loop step is recorded once (not at
/// `ActivateAbility`, which accepted nothing) and the mana-undo window closes.
#[test]
fn a_settled_lock_with_no_election_accepts_the_activation_exactly_once() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Theirs", 1, 1);
    let land = s.add_basic_land(P0, ManaColor::White);
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Engine",
            "{3}: Tap target creature. Create a 1/1 white Soldier creature token.",
        )
        .id();
    let mut r = s.build();
    r.state_mut().loop_detection = engine::types::game_state::LoopDetectionMode::On;
    tap_land_for_mana(&mut r, land);
    // The manual land tap opened this controller's loop period; an accepted
    // activation appends one step to it.
    let period = r.state().last_loop_action_sequence.len();

    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("Hojo makes {3} cost {1}");
    assert_eq!(
        r.state().last_loop_action_sequence.len(),
        period,
        "not accepted before its cost locks"
    );
    assert!(
        r.state().lands_tapped_for_mana.contains_key(&P0),
        "the window stays open until the lock"
    );
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(own)],
        },
    )
    .expect("targets settle, and the lock runs");
    assert_eq!(
        r.state().last_loop_action_sequence.len(),
        period + 1,
        "accepted exactly once, at the settlement lock"
    );
    assert!(
        !r.state().lands_tapped_for_mana.contains_key(&P0),
        "accepting closed the mana-undo window"
    );
    finish(&mut r, &[]);
    assert_eq!(
        r.state().last_loop_action_sequence.len(),
        period + 1,
        "and not again"
    );
}

/// HIGH 2's election twin: the settlement election accepts nothing at its
/// prompt and accepts exactly once on its resume.
#[test]
fn a_settlement_election_accepts_the_activation_once_on_its_resume() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_artifact_from_oracle(P0, "Training Grounds", GROUNDS_FLOORED);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Bear", 2, 2);
    let src = s
        .add_creature_from_oracle(
            P0,
            "Tapper",
            2,
            2,
            "{3}: Tap target creature. Create a 1/1 white Soldier creature token.",
        )
        .id();
    mana(&mut s, P0, 5);
    let mut r = s.build();
    unsick(&mut r, src);
    r.state_mut().loop_detection = engine::types::game_state::LoopDetectionMode::On;
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .unwrap();
    select_own_and_reach_the_election(&mut r, own);
    assert!(
        r.state().last_loop_action_sequence.is_empty(),
        "the settlement prompt accepted nothing"
    );
    elect_total(&mut r, 1).expect("the election resumes");
    finish(&mut r, &[]);
    assert_eq!(
        r.state().last_loop_action_sequence.len(),
        1,
        "accepted once, on resume"
    );
}

// ---------------------------------------------------------------------------
// The election at settlement, and RT-3: its round trip.
// ---------------------------------------------------------------------------

/// `{3}` under Hojo's unfloored `-2` and a floored Training Grounds `-2`,
/// targeting your own creature: Grounds first locks `{0}`, Hojo first `{1}`.
fn settlement_election_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_artifact_from_oracle(P0, "Training Grounds", GROUNDS_FLOORED);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Bear", 2, 2);
    let src = s
        .add_creature_from_oracle(P0, "Tapper", 2, 2, "{3}: Tap target creature.")
        .id();
    mana(&mut s, P0, 5);
    let mut r = s.build();
    unsick(&mut r, src);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("activation starts");
    (r, src, own)
}

fn select_own_and_reach_the_election(r: &mut GameRunner, own: ObjectId) -> WaitingFor {
    let wf = act(
        r,
        GameAction::SelectTargets {
            targets: vec![object(own)],
        },
    )
    .expect("targets settle");
    assert_eq!(election_totals(&wf), vec![0, 1], "CR 601.2f: two totals");
    wf
}

#[test]
fn the_caster_elects_the_reduction_order_at_target_settlement() {
    for (elected, paid) in [(0, 0), (1, 1)] {
        let (mut r, _, own) = settlement_election_board();
        let before = pool(&r, P0);
        let wf = select_own_and_reach_the_election(&mut r, own);
        let WaitingFor::OrderCostReductions { pending_cast, .. } = &wf else {
            unreachable!()
        };
        let snapshot = pending_cast
            .activation_cost_snapshot
            .as_deref()
            .expect("the prompt carries the carrier");
        assert!(matches!(
            snapshot.lock,
            ActivationCostLock::Open {
                point: ActivationCostLockPoint::TargetSettlement
            }
        ));
        assert!(
            snapshot.settlement_tail.is_some(),
            "the resume names its tail"
        );
        elect_total(&mut r, elected).expect("the election resumes");
        finish(&mut r, &[]);
        assert_eq!(before - pool(&r, P0), paid, "elected {elected}");
    }
}

/// RT-3, the prerequisite's obligation: serialize at the settlement prompt,
/// restore through the persisted pipeline, then answer. The answer re-enters no
/// announcement step (no mode, X or target prompt, no new targeting event), pays
/// the elected total, and the stack entry keeps its original target.
#[test]
fn a_settlement_election_survives_a_restore_and_never_re_announces() {
    let (mut r, src, own) = settlement_election_board();
    let before = pool(&r, P0);
    select_own_and_reach_the_election(&mut r, own);
    let mut restored = round_trip(&r);
    assert!(matches!(
        restored.state().waiting_for,
        WaitingFor::OrderCostReductions { .. }
    ));

    let WaitingFor::OrderCostReductions { outcomes, .. } = restored.state().waiting_for.clone()
    else {
        unreachable!()
    };
    let costly = outcomes
        .iter()
        .find(|o| o.locked_cost.mana_value() == 1)
        .unwrap();
    let result = restored
        .act(GameAction::OrderCostReductions {
            order: costly.order.clone(),
            hybrid_announcement: Vec::new(),
        })
        .expect("the restored election resumes");
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::BecomesTarget { .. })),
        "no targeting event is re-emitted"
    );
    let mut prompts = vec![result.waiting_for.clone()];
    loop {
        match restored.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => break,
            WaitingFor::ManaPayment { .. } => {
                prompts.push(restored.state().waiting_for.clone());
                act(&mut restored, GameAction::PassPriority).unwrap();
            }
            other => panic!("the resume must go straight to payment, got {other:?}"),
        }
    }
    assert!(!prompts.iter().any(|w| matches!(
        w,
        WaitingFor::AbilityModeChoice { .. }
            | WaitingFor::ChooseXValue { .. }
            | WaitingFor::TargetSelection { .. }
    )));
    assert_eq!(before - pool(&restored, P0), 1, "the elected total");
    let entry = restored.state().stack.back().expect("placed");
    assert_eq!(entry.source_id, src);
    let engine::types::game_state::StackEntryKind::ActivatedAbility { ability, .. } = &entry.kind
    else {
        panic!("an activated ability");
    };
    assert_eq!(ability.targets, vec![object(own)], "the original target");
}

/// A costlier elected total that turns out unpayable reverses the activation
/// at the outer action boundary (the prerequisite's typed outcome).
#[test]
fn an_unpayable_elected_total_at_settlement_reverses_the_activation() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_artifact_from_oracle(P0, "Training Grounds", GROUNDS_FLOORED);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Bear", 2, 2);
    let src = s
        .add_creature_from_oracle(P0, "Tapper", 2, 2, "{3}: Tap target creature.")
        .id();
    let mut r = s.build();
    unsick(&mut r, src);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("the {0} default makes it affordable with no mana");
    select_own_and_reach_the_election(&mut r, own);
    let result = r
        .act(GameAction::OrderCostReductions {
            order: match &r.state().waiting_for {
                WaitingFor::OrderCostReductions { outcomes, .. } => outcomes
                    .iter()
                    .find(|o| o.locked_cost.mana_value() == 1)
                    .unwrap()
                    .order
                    .clone(),
                _ => unreachable!(),
            },
            hybrid_announcement: Vec::new(),
        })
        .expect("a reversal is a result, not an error");
    assert!(!result.disposition.is_applied(), "typed reversal");
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(stack_len(&r), 0);
    assert!(
        r.state().abilities_activated_this_turn_by_player.is_empty(),
        "a reversed activation leaves no journal row"
    );
    assert!(!r.state().objects[&own].tapped);
}

// ---------------------------------------------------------------------------
// Breya: a modal activation's carrier crosses AbilityModeChoice (RT-1), and a
// frozen v78 payload keeps its announcement fold (RT-2).
// ---------------------------------------------------------------------------

struct BreyaBoard {
    runner: GameRunner,
    breya: ObjectId,
    fodder: [ObjectId; 2],
    merfolk: ObjectId,
}

fn breya_board(kopala: bool) -> BreyaBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_artifact_from_oracle(P0, "Unfloored Reducer", ONE_LESS_UNFLOORED);
    let breya = s
        .add_creature_from_oracle(P0, "Breya, Etherium Shaper", 4, 4, BREYA)
        .id();
    let a1 = s
        .add_artifact_from_oracle(P0, "Bauble A", "{T}: You gain 1 life.")
        .id();
    let a2 = s
        .add_artifact_from_oracle(P0, "Bauble B", "{T}: You gain 1 life.")
        .id();
    if kopala {
        s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
            .with_subtypes(vec!["Merfolk", "Wizard"]);
    }
    let merfolk = s
        .add_creature(P1, "Merfolk", 5, 5)
        .with_subtypes(vec!["Merfolk"])
        .id();
    s.add_creature(P1, "Bear", 5, 5);
    mana(&mut s, P0, 8);
    let mut runner = s.build();
    unsick(&mut runner, breya);
    BreyaBoard {
        runner,
        breya,
        fodder: [a1, a2],
        merfolk,
    }
}

fn breya_mode_then_pay(
    r: &mut GameRunner,
    mode: usize,
    target: Option<ObjectId>,
    fodder: &[ObjectId],
) {
    act(
        r,
        GameAction::SelectModes {
            indices: vec![mode],
        },
    )
    .expect("mode chosen");
    if let Some(target) = target {
        if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
            act(
                r,
                GameAction::SelectTargets {
                    targets: vec![object(target)],
                },
            )
            .expect("target chosen");
        }
    }
    finish(r, fodder);
}

/// RT-1: Breya's `{2}` + Kopala's `{2}` - the unfloored `{1}` = `{3}` for
/// mode 2 on the Merfolk. The carrier crosses the mode prompt `Open`, with the
/// printed cost, and survives a restore there. Marker lost pays 2; the old
/// two-pass fold 1; Kopala only 4.
#[test]
fn a_modal_activations_open_carrier_survives_a_restore_at_mode_choice() {
    for round_trip_at_mode_choice in [false, true] {
        let BreyaBoard {
            mut runner,
            breya,
            fodder,
            merfolk,
            ..
        } = breya_board(true);
        let before = pool(&runner, P0);
        let wf = act(
            &mut runner,
            GameAction::ActivateAbility {
                source_id: breya,
                ability_index: 0,
            },
        )
        .expect("Breya activates");
        let WaitingFor::AbilityModeChoice {
            ability_cost,
            activation_cost_snapshot,
            ..
        } = &wf
        else {
            panic!("expected AbilityModeChoice, got {wf:?}");
        };
        let snapshot = activation_cost_snapshot.as_deref().expect("carrier");
        assert!(
            matches!(
                snapshot.lock,
                ActivationCostLock::Open {
                    point: ActivationCostLockPoint::TargetSettlement
                }
            ),
            "reach guard: the lock waits for the targets"
        );
        let printed_generic = match ability_cost {
            Some(AbilityCost::Composite { costs }) => costs.iter().find_map(|c| match c {
                AbilityCost::Mana {
                    cost: ManaCost::Cost { generic, .. },
                } => Some(*generic),
                _ => None,
            }),
            _ => None,
        };
        assert_eq!(printed_generic, Some(2), "the printed, unfolded {{2}}");
        if round_trip_at_mode_choice {
            runner = round_trip(&runner);
        }
        breya_mode_then_pay(&mut runner, 1, Some(merfolk), &fodder);
        assert_eq!(
            before - pool(&runner, P0),
            3,
            "round trip: {round_trip_at_mode_choice}"
        );
    }
}

/// R4 / M1: the chosen mode declares no target, so no settlement follows; the
/// deferred lock runs at the mode choice, with the target-independent `{1}`-less.
#[test]
fn a_deferred_modal_activation_whose_chosen_mode_has_no_target_locks_at_mode_choice() {
    let BreyaBoard {
        mut runner,
        breya,
        fodder,
        ..
    } = breya_board(true);
    let before = pool(&runner, P0);
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: breya,
            ability_index: 0,
        },
    )
    .unwrap();
    perf_counters::reset();
    breya_mode_then_pay(&mut runner, 2, None, &fodder);
    let routes = perf_counters::activation_cost_route_snapshot();
    assert_eq!(before - pool(&runner, P0), 1, "{{2}} - {{1}}");
    assert_eq!(
        routes.open_settlements, 1,
        "reach guard: M1 locked the open carrier"
    );
}

/// RT-2: a paused `AbilityModeChoice` written by the protocol-78 serializer
/// (see `breya_mode_choice_v78.json.provenance`) carries no carrier and a cost
/// v78 already folded at announcement. It resumes folded exactly once.
#[test]
fn a_frozen_v78_mode_choice_keeps_its_announcement_fold() {
    let json = include_str!("../fixtures/activation_cost/breya_mode_choice_v78.json");
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let data = &value["state"]["waiting_for"]["data"];
    assert_eq!(value["state"]["waiting_for"]["type"], "AbilityModeChoice");
    let mut keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "ability_cost",
            "ability_index",
            "is_activated",
            "modal",
            "mode_abilities",
            "player",
            "source_id"
        ],
        "the v78 key set: no carrier"
    );
    assert_eq!(
        data["ability_cost"]["costs"][0]["cost"]["generic"], 1,
        "v78 folded the {{1}}-less at announcement"
    );

    let mut runner = restore_json(json);
    let find = |runner: &GameRunner, name: &str| {
        runner
            .state()
            .objects
            .values()
            .find(|o| o.name == name && o.zone == Zone::Battlefield)
            .map(|o| o.id)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let bear = find(&runner, "Bear");
    let fodder = [find(&runner, "Bauble A"), find(&runner, "Bauble B")];
    let before = pool(&runner, P0);
    perf_counters::reset();
    breya_mode_then_pay(&mut runner, 1, Some(bear), &fodder);
    assert_eq!(
        before - pool(&runner, P0),
        1,
        "folded once, at announcement, by v78; folding again would pay 0"
    );
    assert_eq!(
        perf_counters::activation_cost_route_snapshot().carrierless_settlements,
        1,
        "reach guard: the carrierless legacy pending reached settlement"
    );
}

// ---------------------------------------------------------------------------
// The turn's activation journal, written at placement (M5a, L1, L2).
// ---------------------------------------------------------------------------

fn once_per_turn_untargeted_reducer() -> StaticDefinition {
    StaticDefinition::new(StaticMode::ReduceAbilityCost {
        mode: CostModifyMode::Reduce,
        keyword: "activated".to_string(),
        amount: 2,
        minimum_mana: None,
        dynamic_count: None,
        exemption: ActivationExemption::None,
        activator: None,
        targets: None,
        frequency: Some(CastFrequency::OncePerTurn),
    })
}

/// M5a: an untargeted once-per-turn discount that folds `{2}` to `{0}` reaches
/// the stack through the direct push, which must journal the activation too.
/// Built at the building-block level: the parser declines this shape
/// (Tezzeret).
#[test]
fn a_once_per_turn_discount_is_spent_by_a_zero_cost_direct_push() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_artifact_from_oracle(P0, "Reducer", "")
        .with_static_definition(once_per_turn_untargeted_reducer());
    let src = s
        .add_artifact_from_oracle(P0, "Lifestone", "{2}: You gain 1 life.")
        .id();
    mana(&mut s, P0, 4);
    let mut r = s.build();
    let b0 = pool(&r, P0);
    r.activate(src, 0).resolve();
    let b1 = pool(&r, P0);
    r.activate(src, 0).resolve();
    let b2 = pool(&r, P0);
    assert_eq!((b0 - b1, b1 - b2), (0, 2), "first free, then full price");
}

const LOYALTY_TAP: &str = "+1: Tap target creature.";
const LOYALTY_GAIN: &str = "+1: You gain 1 life.";

/// L1: a loyalty ability that qualifies for Hojo (it targets a creature you
/// control) is the turn's first such activation, so the next one pays full
/// even though a reduction can't touch a bare loyalty cost. One row per
/// loyalty route.
fn after_loyalty_paid(loyalty: Option<(&str, usize)>) -> usize {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own_creatures: Vec<ObjectId> = match loyalty {
        Some((_, n)) => (0..n.saturating_sub(1))
            .map(|i| s.add_creature(P0, &format!("Own {i}"), 1, 1).id())
            .collect(),
        None => vec![s.add_creature(P0, "Own", 1, 1).id()],
    };
    let walker = s
        .add_planeswalker_from_oracle(
            P0,
            "Test Walker",
            "Test",
            3,
            loyalty.map_or(LOYALTY_GAIN, |(text, _)| text),
        )
        .id();
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{2}: Tap target creature.")
        .id();
    mana(&mut s, P0, 4);
    let mut r = s.build();
    let hojo = r
        .state()
        .objects
        .values()
        .find(|o| o.name == "Professor Hojo")
        .unwrap()
        .id;
    if let Some((text, _)) = loyalty {
        act(
            &mut r,
            GameAction::ActivateAbility {
                source_id: walker,
                ability_index: 0,
            },
        )
        .expect("loyalty activation");
        if let WaitingFor::TargetSelection { pending_cast, .. } = &r.state().waiting_for {
            // r5c #1: the loyalty fast path captured the draft before this
            // prompt and before its counter cost, so settlement only adds
            // the committed targets to it.
            let draft = pending_cast
                .ability
                .activation_record
                .as_deref()
                .expect("the interactive loyalty route carries its draft into the prompt");
            assert!(
                draft.is_loyalty_ability && draft.activator == P0 && draft.source == walker,
                "{draft:?}"
            );
            act(
                &mut r,
                GameAction::SelectTargets {
                    targets: vec![object(hojo)],
                },
            )
            .expect("loyalty target");
        }
        let rows = r
            .state()
            .abilities_activated_this_turn_by_player
            .get(&P0)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            rows.len(),
            1,
            "the loyalty activation is journaled at placement"
        );
        assert!(rows[0].is_loyalty_ability, "{:?}", rows[0]);
        if text == LOYALTY_GAIN {
            assert!(rows[0].targets.is_empty(), "{:?}", rows[0]);
        } else {
            assert!(
                matches!(
                    rows[0].targets.as_slice(),
                    [ActivationTargetFact::Object { id, lki }] if *id == hojo && lki.controller == P0
                ),
                "journaled with its committed target as it was: {:?}",
                rows[0]
            );
        }
        // Resolve the loyalty ability and Hojo's trigger.
        while !r.state().stack.is_empty() {
            act(&mut r, GameAction::PassPriority).unwrap();
            act(&mut r, GameAction::PassPriority).unwrap();
        }
    }
    let target = own_creatures.first().copied().unwrap_or(hojo);
    let before = pool(&r, P0);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .unwrap();
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        act(
            &mut r,
            GameAction::SelectTargets {
                targets: vec![object(target)],
            },
        )
        .unwrap();
    }
    finish(&mut r, &[]);
    before - pool(&r, P0)
}

#[test]
fn a_qualifying_loyalty_activation_spends_the_once_per_turn_discount() {
    // Every board also has P1's creature-free side, so the only creatures are
    // P0's: Hojo plus the listed extras.
    assert_eq!(
        after_loyalty_paid(None),
        0,
        "control: the discount is unspent"
    );
    assert_eq!(
        after_loyalty_paid(Some((LOYALTY_TAP, 2))),
        2,
        "L1a: interactive loyalty target (Hojo or the other creature)"
    );
    assert_eq!(
        after_loyalty_paid(Some((LOYALTY_TAP, 1))),
        2,
        "L1b: the loyalty ability auto-targets Hojo, the only creature"
    );
    assert_eq!(
        after_loyalty_paid(Some((LOYALTY_GAIN, 1))),
        0,
        "L1c: an untargeted loyalty ability doesn't qualify"
    );
}

/// L2: two activations of one ability share source and index. Each journal row
/// holds the targets of the entry it was captured for, not the older entry
/// beneath it: the first (an opponent's creature) doesn't qualify, the second
/// (your own) does, so the third pays full price.
#[test]
fn consumption_reads_the_entry_just_placed_not_an_older_one_beneath_it() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    let theirs = s.add_creature(P1, "Theirs", 1, 1).id();
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{2}: Tap target creature.")
        .id();
    mana(&mut s, P0, 8);
    let mut r = s.build();
    let mut paid = Vec::new();
    for target in [theirs, own, own] {
        let before = pool(&r, P0);
        act(
            &mut r,
            GameAction::ActivateAbility {
                source_id: src,
                ability_index: 0,
            },
        )
        .unwrap();
        act(
            &mut r,
            GameAction::SelectTargets {
                targets: vec![object(target)],
            },
        )
        .unwrap();
        finish(&mut r, &[]);
        paid.push(before - pool(&r, P0));
        // Hold priority: the earlier entries stay on the stack beneath.
    }
    assert!(stack_len(&r) >= 3, "three entries stacked");
    assert_eq!(paid, vec![2, 0, 2]);
}

// ---------------------------------------------------------------------------
// M5b: the common case never walks assignments.
// ---------------------------------------------------------------------------

#[test]
fn a_discount_affordable_whatever_the_targets_needs_no_assignment_walk() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    for i in 0..60 {
        s.add_creature(if i % 2 == 0 { P0 } else { P1 }, &format!("C{i}"), 1, 1);
    }
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{3}: Tap up to three target creatures.")
        .id();
    mana(&mut s, P0, 8);
    let r = s.build();
    perf_counters::reset();
    let started = std::time::Instant::now();
    assert!(can_activate_ability_now(r.state(), P0, src, 0));
    assert_eq!(
        perf_counters::activation_cost_route_snapshot().window_searches,
        0,
        "payable at the worst case: decided without a walk"
    );
    assert!(started.elapsed().as_secs() < 5, "was 217 s before the fix");
}

// ---------------------------------------------------------------------------
// N: the carrier rule.
// ---------------------------------------------------------------------------

/// N1: a fresh target-gated activation reaches settlement with an `Open`
/// carrier, and its lock is recorded at target settlement.
#[test]
fn a_fresh_target_gated_activation_settles_an_open_carrier() {
    let (mut r, src, merfolk, _, fodder) = sacrifice_board(4);
    perf_counters::reset();
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .unwrap();
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(merfolk)],
        },
    )
    .unwrap();
    let routes = perf_counters::activation_cost_route_snapshot();
    assert_eq!(routes.open_settlements, 1, "{routes:?}");
    assert_eq!(routes.carrierless_settlements, 0);
    finish(&mut r, &[fodder]);
    assert_eq!(pool(&r, P0), 0, "{{2}} + Kopala's {{2}}");
}

/// N2: a non-loyalty target-gated activation that reaches settlement with no
/// carrier at all is refused, not priced without its target-gated modifiers.
#[test]
fn a_carrierless_target_gated_activation_is_refused_at_settlement() {
    let (mut r, src, merfolk, _, _) = sacrifice_board(4);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .unwrap();
    if let WaitingFor::TargetSelection { pending_cast, .. } = &mut r.state_mut().waiting_for {
        pending_cast.activation_cost_snapshot = None;
    } else {
        panic!("expected target selection");
    }
    let refused = act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(merfolk)],
        },
    );
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(pool(&r, P0), 4);
    assert_eq!(stack_len(&r), 0);
}

/// The lock point recorded on a settled carrier.
#[test]
fn a_deferred_lock_records_target_settlement_as_its_point() {
    let (mut r, src, merfolk, _, _) = sacrifice_board(4);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .unwrap();
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(merfolk)],
        },
    )
    .unwrap();
    let pending_cast = match &r.state().waiting_for {
        WaitingFor::PayCost {
            resume: CostResume::Spell { spell } | CostResume::SpellCost { spell, .. },
            ..
        } => spell.clone(),
        other => panic!("expected the sacrifice prompt, got {other:?}"),
    };
    let snapshot = pending_cast.activation_cost_snapshot.as_deref().unwrap();
    assert!(matches!(
        snapshot.lock,
        ActivationCostLock::Locked {
            point: ActivationCostLockPoint::TargetSettlement,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// G: every cost-work entry refuses an unlocked carrier (G1), and each row's
// board really reaches its entry (G2).
// ---------------------------------------------------------------------------

use engine::game::casting::ActivationCostGuardSite;
use engine::types::counter::CounterType;
use engine::types::game_state::{CounterCostChoice, ShardChoice};

/// Re-opens every live activation carrier: the shape a route that bypassed
/// target settlement would leave.
fn open_live_carriers(r: &mut GameRunner) -> usize {
    fn open(
        snapshot: &mut Option<Box<engine::types::casting_costs::ActivationCostSnapshot>>,
    ) -> usize {
        match snapshot.as_deref_mut() {
            Some(snapshot) => {
                snapshot.lock = ActivationCostLock::Open {
                    point: ActivationCostLockPoint::TargetSettlement,
                };
                1
            }
            None => 0,
        }
    }
    let state = r.state_mut();
    let mut opened = 0;
    if let Some(pending) = state.pending_cast.as_deref_mut() {
        opened += open(&mut pending.activation_cost_snapshot);
    }
    if let WaitingFor::PayCost {
        resume: CostResume::Spell { spell } | CostResume::SpellCost { spell, .. },
        ..
    } = &mut state.waiting_for
    {
        opened += open(&mut spell.activation_cost_snapshot);
    }
    opened
}

/// One G row: `build` reaches a cost prompt with a LOCKED carrier and names
/// the action that continues it.
fn guard_row(site: ActivationCostGuardSite, build: impl Fn() -> (GameRunner, GameAction)) {
    guard_reach(site, &build);
    guard_refusal(site, &build);
}

/// G2 alone: the board reaches the entry with a locked carrier.
fn guard_reach(site: ActivationCostGuardSite, build: &impl Fn() -> (GameRunner, GameAction)) {
    let (mut r, action) = build();
    perf_counters::reset();
    r.act(action.clone())
        .unwrap_or_else(|e| panic!("{site:?}: the locked control must proceed: {e:?}"));
    let reaches = perf_counters::activation_cost_route_snapshot().guard_reaches;
    assert!(
        reaches[site as usize] > 0,
        "{site:?}: the board must reach its entry: {reaches:?}"
    );
}

/// G1: the same drive with the carrier re-opened is refused with no side effect.
fn guard_refusal(site: ActivationCostGuardSite, build: &impl Fn() -> (GameRunner, GameAction)) {
    let (mut r, action) = build();
    assert!(
        open_live_carriers(&mut r) > 0,
        "{site:?}: a carrier to reopen"
    );
    let before = serde_json::to_value(r.state()).unwrap();
    let refused = r.act(action);
    let message = format!("{refused:?}");
    assert!(
        refused.is_err() && message.contains("must be locked"),
        "{site:?}: an unlocked carrier is refused: {message}"
    );
    assert_eq!(
        serde_json::to_value(r.state()).unwrap(),
        before,
        "{site:?}: nothing paid, moved or stacked"
    );
}

/// `{2}` plus `extra` as the ability's cost, on a board with a target and the
/// cost's fodder; returns the runner at the cost prompt after targeting.
fn at_cost_prompt(cost: &str, pool: usize, fodder_counters: u32) -> (GameRunner, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let fodder = s.add_creature(P0, "Fodder", 1, 1).id();
    if fodder_counters > 0 {
        s.with_counter(fodder, CounterType::Plus1Plus1, fodder_counters);
    }
    let bear = s.add_creature(P1, "Bear", 2, 2).id();
    let src = s
        .add_artifact_from_oracle(P0, "Engine", &format!("{cost}: Tap target creature."))
        .id();
    mana(&mut s, P0, pool);
    let mut r = s.build();
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("activation starts");
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        act(
            &mut r,
            GameAction::SelectTargets {
                targets: vec![object(bear)],
            },
        )
        .expect("target chosen");
    }
    (r, fodder)
}

#[test]
fn every_cost_work_entry_refuses_an_unlocked_carrier() {
    guard_row(ActivationCostGuardSite::PushToStack, || {
        let (r, fodder) = at_cost_prompt("{1}, Sacrifice a creature", 1, 0);
        (
            r,
            GameAction::SelectCards {
                cards: vec![fodder],
            },
        )
    });
    guard_row(ActivationCostGuardSite::ReturnToHand, || {
        let (r, fodder) = at_cost_prompt(
            "{1}, Return a creature you control to its owner's hand",
            1,
            0,
        );
        (
            r,
            GameAction::SelectCards {
                cards: vec![fodder],
            },
        )
    });
    guard_row(ActivationCostGuardSite::RemoveCounter, || {
        let (r, fodder) = at_cost_prompt(
            "{1}, Remove a +1/+1 counter from a creature you control",
            1,
            1,
        );
        (
            r,
            GameAction::SelectCards {
                cards: vec![fodder],
            },
        )
    });
    guard_row(ActivationCostGuardSite::RemoveCounterDistribution, || {
        let (r, fodder) = at_cost_prompt(
            "{1}, Remove two +1/+1 counters from among creatures you control",
            1,
            2,
        );
        (
            r,
            GameAction::ChooseRemoveCounterCostDistribution {
                distribution: vec![CounterCostChoice {
                    object_id: fodder,
                    counter_type: CounterType::Plus1Plus1,
                    count: 2,
                }],
            },
        )
    });
    guard_row(ActivationCostGuardSite::PhyrexianResume, || {
        let (r, _) = at_cost_prompt("{W/P}", 0, 0);
        (
            r,
            GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayLife],
            },
        )
    });
}

/// The entry an ordinary target-first mana payment finalizes through is
/// reached inside the settling action itself, after the settlement lock, so no
/// live carrier exists between the two to reopen: G2 only, with G1 held by the
/// census's structural check that the entry calls the guard.
#[test]
fn a_settled_mana_payment_reaches_the_mana_resume_guard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let bear = s.add_creature(P1, "Bear", 2, 2).id();
    // A second legal target, so targets are chosen interactively.
    s.add_creature(P1, "Other Bear", 2, 2);
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{2}: Tap target creature.")
        .id();
    mana(&mut s, P0, 2);
    let mut r = s.build();
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .unwrap();
    guard_reach(ActivationCostGuardSite::ManaResume, &|| {
        (
            GameRunner::from_state(r.state().clone()),
            GameAction::SelectTargets {
                targets: vec![object(bear)],
            },
        )
    });
}

/// The untargeted direct payment, reached for real. An untargeted cost never
/// defers its lock, so no carrier it sees can be `Open`: G1 for it is the
/// census's structural check. (`ReturnAfterAutomatic` is reached only after a
/// replacement pauses a returned permanent's move, past the guarded return
/// prompt; it too is held by the census.)
#[test]
fn the_untargeted_direct_payment_reaches_its_guard() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let src = s
        .add_artifact_from_oracle(P0, "Lifestone", "{T}: You gain 1 life.")
        .id();
    let mut r = s.build();
    perf_counters::reset();
    r.activate(src, 0).resolve();
    let reaches = perf_counters::activation_cost_route_snapshot().guard_reaches;
    assert!(
        reaches[ActivationCostGuardSite::DirectPay as usize] > 0,
        "{reaches:?}"
    );
}

// ---------------------------------------------------------------------------
// R: every target-settlement route locks the deferred carrier exactly once,
// before payment. Kopala's `{2}` tax is the observable: it exists only if the
// route settled with the committed targets.
// ---------------------------------------------------------------------------

struct RouteBoard {
    runner: GameRunner,
    src: ObjectId,
    merfolk: ObjectId,
    bear: Option<ObjectId>,
}

/// P1 controls Kopala (itself a Merfolk) and, when `bear`, an untaxed Bear.
fn route_board(ability: &str, bear: bool) -> RouteBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let merfolk = s
        .add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
        .with_subtypes(vec!["Merfolk", "Wizard"])
        .id();
    let bear = bear.then(|| s.add_creature(P1, "Bear", 2, 2).id());
    let src = s.add_artifact_from_oracle(P0, "Engine", ability).id();
    mana(&mut s, P0, 8);
    RouteBoard {
        runner: s.build(),
        src,
        merfolk,
        bear,
    }
}

fn start(board: &mut RouteBoard) -> WaitingFor {
    perf_counters::reset();
    act(
        &mut board.runner,
        GameAction::ActivateAbility {
            source_id: board.src,
            ability_index: 0,
        },
    )
    .expect("activation starts")
}

fn assert_route(board: &mut RouteBoard, route: &str, expected_paid: usize) {
    finish(&mut board.runner, &[]);
    let routes = perf_counters::activation_cost_route_snapshot();
    assert_eq!(
        routes.open_settlements, 1,
        "{route}: the deferred lock ran once, at settlement: {routes:?}"
    );
    assert_eq!(8 - pool(&board.runner, P0), expected_paid, "{route}");
}

#[test]
fn every_target_settlement_route_prices_the_committed_targets() {
    // R2: one slot at a time.
    let mut b = route_board("{2}: Tap target creature.", true);
    start(&mut b);
    act(
        &mut b.runner,
        GameAction::ChooseTarget {
            target: Some(object(b.merfolk)),
        },
    )
    .expect("R2 target");
    assert_route(&mut b, "R2 ChooseTarget", 4);

    // R3: a modal activation whose chosen mode's only legal target is taxed.
    let mut b = route_board(
        "{2}: Choose one \u{2014}\n\u{2022} Tap target creature an opponent controls.\n\u{2022} You gain 1 life.",
        false,
    );
    start(&mut b);
    act(&mut b.runner, GameAction::SelectModes { indices: vec![0] }).expect("R3 mode");
    assert_route(&mut b, "R3 modal auto-target", 4);

    // R5: a single legal target is selected automatically.
    let mut b = route_board("{2}: Tap target creature an opponent controls.", false);
    start(&mut b);
    assert_route(&mut b, "R5 auto-target", 4);

    // R6: X is announced before the X-dependent targets are chosen.
    let mut b = route_board("{X}: Tap X target creatures.", true);
    let wf = start(&mut b);
    assert!(matches!(wf, WaitingFor::ChooseXValue { .. }), "{wf:?}");
    act(&mut b.runner, GameAction::ChooseX { value: 1 }).expect("R6 X");
    act(
        &mut b.runner,
        GameAction::SelectTargets {
            targets: vec![object(b.merfolk)],
        },
    )
    .expect("R6 target");
    assert_route(&mut b, "R6 deferred X", 3);

    // R9: divided damage across the taxed Merfolk and the Bear.
    let mut b = route_board(
        "{2}: Engine deals 2 damage divided as you choose among one or two targets.",
        true,
    );
    let bear = b.bear.unwrap();
    start(&mut b);
    act(
        &mut b.runner,
        GameAction::SelectTargets {
            targets: vec![object(b.merfolk), object(bear)],
        },
    )
    .expect("R9 targets");
    assert!(
        matches!(
            b.runner.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "R9 reach guard: the division is chosen before payment"
    );
    {
        act(
            &mut b.runner,
            GameAction::DistributeAmong {
                distribution: vec![(object(b.merfolk), 1), (object(bear), 1)],
            },
        )
        .expect("R9 distribution");
    }
    assert_route(&mut b, "R9 divided", 4);
}

// ---------------------------------------------------------------------------
// O5 / O6: optional target slots. The empty completion is always legal; with
// Hojo's discount it is also the one completion that is NOT affordable.
// ---------------------------------------------------------------------------

fn up_to_three_offered(own_creature_targetable: bool) -> (bool, u64) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    for i in 0..4 {
        s.add_creature(P1, &format!("Theirs {i}"), 1, 1);
    }
    let text = if own_creature_targetable {
        "{3}: Tap up to three target creatures."
    } else {
        "{3}: Tap up to three target creatures an opponent controls."
    };
    let src = s.add_artifact_from_oracle(P0, "Tapper", text).id();
    mana(&mut s, P0, 1);
    let r = s.build();
    perf_counters::reset();
    let offered = can_activate_ability_now(r.state(), P0, src, 0);
    (
        offered,
        perf_counters::activation_cost_route_snapshot().window_searches,
    )
}

/// O5: only a completion that includes Hojo itself (a creature you control)
/// costs `{1}`; the empty completion costs `{3}`. The walk must go past the
/// refused empty completion to find it. O6: with no creature you control
/// targetable, every completion costs `{3}`, even though the empty one is legal.
#[test]
fn optional_slots_are_searched_past_an_unaffordable_empty_completion() {
    assert_eq!(
        up_to_three_offered(true),
        (true, 1),
        "O5: Payable, by walking"
    );
    assert_eq!(
        up_to_three_offered(false),
        (false, 1),
        "O6: Unpayable, by walking"
    );
}

/// R9 + accept-at-lock: a divided activation waits in `DistributeAmong` with
/// its lock still open (the division comes before settlement), so it is
/// accepted at the settlement lock AFTER the split: not at `ActivateAbility`,
/// not at the target choice, and not skipped.
#[test]
fn a_divided_activation_is_accepted_at_its_settlement_lock_after_the_split() {
    let mut b = route_board(
        "{2}: Engine deals 2 damage divided as you choose among one or two targets. Create a 1/1 white Soldier creature token.",
        true,
    );
    b.runner.state_mut().loop_detection = engine::types::game_state::LoopDetectionMode::On;
    let bear = b.bear.unwrap();
    start(&mut b);
    assert!(b.runner.state().last_loop_action_sequence.is_empty());
    act(
        &mut b.runner,
        GameAction::SelectTargets {
            targets: vec![object(b.merfolk), object(bear)],
        },
    )
    .expect("targets");
    assert!(
        matches!(
            b.runner.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "reach guard: the division prompt"
    );
    assert!(
        b.runner.state().last_loop_action_sequence.is_empty(),
        "the division comes before the lock: nothing accepted yet"
    );
    act(
        &mut b.runner,
        GameAction::DistributeAmong {
            distribution: vec![(object(b.merfolk), 1), (object(bear), 1)],
        },
    )
    .expect("the split");
    assert_eq!(
        b.runner.state().last_loop_action_sequence.len(),
        1,
        "accepted exactly once, at the settlement lock after the split"
    );
    assert_route(&mut b, "R9 divided, accepted", 4);
}

// ---------------------------------------------------------------------------
// Review round 4: a self rider whose CONDITION reads the chosen target.
// ---------------------------------------------------------------------------

/// Gives `src`'s first ability `rider`, on both its printed and its live
/// ability lists, so a layer pass keeps it.
fn set_cost_rider(
    r: &mut GameRunner,
    src: ObjectId,
    rider: Option<engine::types::ability::CostReduction>,
) {
    let obj = r.state_mut().objects.get_mut(&src).unwrap();
    std::sync::Arc::make_mut(&mut obj.base_abilities)[0].cost_reduction = rider.clone();
    std::sync::Arc::make_mut(&mut obj.abilities)[0].cost_reduction = rider;
}

/// `{3}: Tap target creature`, with a self rider "costs {2} less to activate if
/// the targeted object's mana value is 3 or more", built at the building-block
/// level (no printed card has this shape; the class is any rider whose
/// `QuantityComparison` reads `ObjectManaValue { scope: Target }`).
fn target_mana_value_rider_paid(target_mana_value: u32) -> usize {
    use engine::types::ability::{
        CostReduction, ObjectScope, ParsedCondition, QuantityExpr, QuantityRef,
    };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let target = s
        .add_creature(P1, "Target", 2, 2)
        .with_mana_cost(ManaCost::generic(target_mana_value))
        .id();
    s.add_creature(P1, "Other", 2, 2);
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{3}: Tap target creature.")
        .id();
    mana(&mut s, P0, 8);
    let mut r = s.build();
    let comparator: engine::types::ability::Comparator =
        serde_json::from_str("\"GE\"").expect("the GE comparator");
    set_cost_rider(
        &mut r,
        src,
        Some(CostReduction {
            mode: CostModifyMode::Reduce,
            amount_per: 2,
            count: QuantityExpr::Fixed { value: 1 },
            condition: Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                },
                comparator,
                rhs: QuantityExpr::Fixed { value: 3 },
            }),
        }),
    );
    let before = pool(&r, P0);
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("the activation starts");
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(target)],
        },
    )
    .expect("the target settles");
    finish(&mut r, &[]);
    assert!(r.state().objects[&target].tapped || stack_len(&r) == 1);
    before - pool(&r, P0)
}

/// CR 601.2c + CR 601.2f + CR 602.2b: the rider's condition is decided by the
/// committed target: mana value 4 qualifies ({1}), mana value 1 does not ({3}).
/// Priced without targets, the comparison would read 0 and never qualify.
#[test]
fn a_self_rider_condition_that_reads_the_target_is_priced_with_the_target() {
    assert_eq!(
        target_mana_value_rider_paid(4),
        1,
        "target-positive: {{3}} - {{2}}"
    );
    assert_eq!(
        target_mana_value_rider_paid(1),
        3,
        "target-negative: full price"
    );
}

/// A rider condition that reads a target through a shape this engine can't
/// evaluate against targets is refused, never priced without its target: the
/// ability is not offered, and an explicit activation is rejected before
/// anything is announced.
#[test]
fn a_self_rider_condition_the_engine_cannot_price_with_targets_is_refused() {
    use engine::types::ability::{CostReduction, ParsedCondition, QuantityExpr};
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(P1, "Target", 2, 2);
    let src = s
        .add_artifact_from_oracle(P0, "Tapper", "{3}: Tap target creature.")
        .id();
    mana(&mut s, P0, 8);
    let mut r = s.build();
    set_cost_rider(
        &mut r,
        src,
        Some(CostReduction {
            mode: CostModifyMode::Reduce,
            amount_per: 2,
            count: QuantityExpr::Fixed { value: 1 },
            condition: Some(ParsedCondition::ControlsCreatureWithKeyword {
                controller: engine::types::ability::ControllerRef::TargetPlayer,
                keyword: engine::types::keywords::Keyword::Flying,
            }),
        }),
    );
    assert!(
        !can_activate_ability_now(r.state(), P0, src, 0),
        "not offered"
    );
    let before = serde_json::to_value(r.state()).unwrap();
    assert!(r
        .act(GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        })
        .is_err());
    assert_eq!(serde_json::to_value(r.state()).unwrap(), before);
}

/// Review round 4: a modal `{X}` activation whose chosen mode declares no
/// target. The lock waits for target settlement (another mode could target, and
/// Hojo's discount passes its non-target gates), X is announced after the mode,
/// and the deferred target selection then finds no slot. The lock must still run
/// there, with no target-gated modifier: mode 2 at X = 2 pays `{2}`.
#[test]
fn a_modal_x_activation_whose_chosen_mode_has_no_target_still_locks_its_cost() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    s.add_creature(P0, "Own", 1, 1);
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Modal X",
            "{X}: Choose one \u{2014}\n\u{2022} Tap target creature.\n\u{2022} You gain X life.",
        )
        .id();
    mana(&mut s, P0, 6);
    let mut r = s.build();
    let before = pool(&r, P0);
    perf_counters::reset();
    let wf = act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("the activation starts");
    assert!(matches!(wf, WaitingFor::AbilityModeChoice { .. }), "{wf:?}");
    let wf =
        act(&mut r, GameAction::SelectModes { indices: vec![1] }).expect("the untargeted mode");
    assert!(
        matches!(wf, WaitingFor::ChooseXValue { .. }),
        "reach guard: X after modes: {wf:?}"
    );
    act(&mut r, GameAction::ChooseX { value: 2 }).expect("X = 2");
    finish(&mut r, &[]);
    assert_eq!(stack_len(&r), 1, "the activation reached the stack");
    assert_eq!(before - pool(&r, P0), 2, "X = 2, no target, no discount");
    assert_eq!(
        perf_counters::activation_cost_route_snapshot().open_settlements,
        1,
        "the deferred lock ran once"
    );
}

/// CR 602.2: Bladehold War-Whip's "Equip abilities you activate of other
/// Equipment cost {1} less" discounts another Equipment's equip, not its own.
#[test]
fn bladehold_war_whip_discounts_other_equipment_only() {
    const WAR_WHIP: &str = "Equip abilities you activate of other Equipment cost {1} less to activate.\nEquipped creature has double strike.\nEquip {3}{R}{W}";
    fn equip_paid(own: bool) -> usize {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let whip = s
            .add_artifact_from_oracle(P0, "Bladehold War-Whip", WAR_WHIP)
            .with_subtypes(vec!["Equipment"])
            .id();
        let other = s
            .add_artifact_from_oracle(P0, "Other Blade", "Equip {2}")
            .with_subtypes(vec!["Equipment"])
            .id();
        let bear = s.add_creature(P0, "Bear", 2, 2).id();
        s.with_mana_pool(
            P0,
            [ManaColor::Red, ManaColor::White]
                .into_iter()
                .chain(std::iter::repeat_n(ManaColor::Blue, 6))
                .map(|c| ManaUnit::new(c.into(), ObjectId(0), false, Vec::new()))
                .collect(),
        );
        let mut r = s.build();
        let src = if own { whip } else { other };
        let equip_index = r.state().objects[&src]
            .abilities
            .iter()
            .position(|a| a.ability_tag.is_some())
            .expect("an equip ability");
        let before = pool(&r, P0);
        r.activate(src, equip_index).target_object(bear).resolve();
        before - pool(&r, P0)
    }
    assert_eq!(
        equip_paid(false),
        1,
        "another Equipment's {{2}} equip costs {{1}}"
    );
    assert_eq!(
        equip_paid(true),
        5,
        "War-Whip's own {{3}}{{R}}{{W}} is not discounted"
    );
}

/// Review round 4 MED: a two-mode activation whose SECOND chosen mode owns the
/// target. The chained ability carries that target on its `sub_ability`, so the
/// rider (count and condition) must read the chain's committed targets, not the
/// root mode's. With exactly {1}, "{2} less if the target's mana value is 3 or
/// more" on {3} is offered and pays {1}.
#[test]
fn a_rider_reads_the_target_of_a_later_chosen_mode() {
    use engine::types::ability::{
        CostReduction, ObjectScope, ParsedCondition, QuantityExpr, QuantityRef,
    };
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let target = s
        .add_creature(P1, "Target", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    s.add_creature(P1, "Other", 2, 2);
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Two Modes",
            "{3}: Choose two \u{2014}\n\u{2022} You gain 1 life.\n\u{2022} Tap target creature.",
        )
        .id();
    mana(&mut s, P0, 1);
    let mut r = s.build();
    let comparator: engine::types::ability::Comparator =
        serde_json::from_str("\"GE\"").expect("the GE comparator");
    set_cost_rider(
        &mut r,
        src,
        Some(CostReduction {
            mode: CostModifyMode::Reduce,
            amount_per: 2,
            count: QuantityExpr::Fixed { value: 1 },
            condition: Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                },
                comparator,
                rhs: QuantityExpr::Fixed { value: 3 },
            }),
        }),
    );
    assert!(
        can_activate_ability_now(r.state(), P0, src, 0),
        "offered: the qualifying target makes {{3}} cost {{1}}"
    );
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("the activation starts");
    act(
        &mut r,
        GameAction::SelectModes {
            indices: vec![0, 1],
        },
    )
    .expect("both modes");
    let result = r
        .act(GameAction::SelectTargets {
            targets: vec![object(target)],
        })
        .expect("the target settles");
    assert!(
        result.disposition.is_applied(),
        "settlement must price the second mode's target, not reverse"
    );
    finish(&mut r, &[]);
    assert_eq!(pool(&r, P0), 0, "paid {{1}}");
    assert_eq!(stack_len(&r), 1);
}

/// The rider COUNT twin: "{1} less for each point of the target's mana value",
/// with the target on the second chosen mode. Mana value 2 makes {3} cost {1}.
#[test]
fn a_rider_count_reads_the_target_of_a_later_chosen_mode() {
    use engine::types::ability::{CostReduction, ObjectScope, QuantityExpr, QuantityRef};
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let target = s
        .add_creature(P1, "Target", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    s.add_creature(P1, "Other", 2, 2);
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Two Modes",
            "{3}: Choose two \u{2014}\n\u{2022} You gain 1 life.\n\u{2022} Tap target creature.",
        )
        .id();
    mana(&mut s, P0, 1);
    let mut r = s.build();
    set_cost_rider(
        &mut r,
        src,
        Some(CostReduction {
            mode: CostModifyMode::Reduce,
            amount_per: 1,
            count: QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            },
            condition: None,
        }),
    );
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("offered through the best-case bound");
    act(
        &mut r,
        GameAction::SelectModes {
            indices: vec![0, 1],
        },
    )
    .expect("both modes");
    let result = r
        .act(GameAction::SelectTargets {
            targets: vec![object(target)],
        })
        .expect("the target settles");
    assert!(
        result.disposition.is_applied(),
        "priced from the second mode's target"
    );
    finish(&mut r, &[]);
    assert_eq!(pool(&r, P0), 0, "paid {{1}}");
}

// ---------------------------------------------------------------------------
// Round 5: "first" is the first qualifying activation of the TURN (CR 611.3a;
// the Zimone / Shadow in the Warp rulings), not the first while this Hojo
// object exists.
// ---------------------------------------------------------------------------

const TAPPER: &str = "{2}: Tap target creature.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

fn find_on_battlefield(r: &GameRunner, name: &str) -> ObjectId {
    r.state()
        .objects
        .values()
        .find(|o| o.name == name && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .unwrap_or_else(|| panic!("{name} on the battlefield"))
}

fn activate_on(
    r: &mut GameRunner,
    src: ObjectId,
    target: ObjectId,
    pay_with: &[ObjectId],
) -> usize {
    let before = pool(r, P0);
    let wf = act(
        r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("the activation starts");
    if matches!(wf, WaitingFor::TargetSelection { .. }) {
        act(
            r,
            GameAction::SelectTargets {
                targets: vec![object(target)],
            },
        )
        .expect("the target settles");
    }
    finish(r, pay_with);
    let paid = before - pool(r, P0);
    // Let it (and Hojo's draw trigger) resolve before the next step.
    while !r.state().stack.is_empty() {
        act(r, GameAction::PassPriority).expect("pass");
        if r.state().stack.is_empty() {
            break;
        }
        act(r, GameAction::PassPriority).expect("pass");
    }
    paid
}

fn cast_and_resolve(r: &mut GameRunner, spell: ObjectId, target: Option<ObjectId>) {
    let cast = r.cast(spell);
    let cast = match target {
        Some(target) => cast.target_object(target),
        None => cast,
    };
    cast.resolve();
}

/// H1: an activation that targeted your creature BEFORE Hojo entered is the
/// turn's first qualifying activation, so the next one gets no discount.
#[test]
fn an_activation_before_hojo_entered_is_still_the_first_of_the_turn() {
    for earlier in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        library(&mut s, P0);
        let own = s.add_creature(P0, "Own", 1, 1).id();
        s.add_creature(P1, "Theirs", 1, 1);
        let hojo = s
            .add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
        mana(&mut s, P0, 10);
        let mut r = s.build();
        if earlier {
            assert_eq!(activate_on(&mut r, src, own, &[]), 2, "no Hojo yet");
        }
        cast_and_resolve(&mut r, hojo, None);
        let paid = activate_on(&mut r, src, own, &[]);
        assert_eq!(
            paid,
            if earlier { 2 } else { 0 },
            "earlier qualifying activation: {earlier}"
        );
    }
}

/// H2: a Hojo that leaves and returns, or another Hojo entering later, is not
/// a new turn: the second qualifying activation still pays full. The engine's
/// blink keeps the `ObjectId` and bumps the incarnation (CR 400.7), so the
/// late second Hojo is the case a source-keyed ledger gets wrong.
#[test]
fn hojo_leaving_and_returning_does_not_grant_a_second_first_activation() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Theirs", 1, 1);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let blink = s
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    mana(&mut s, P0, 10);
    let mut r = s.build();
    let hojo = find_on_battlefield(&r, "Professor Hojo");
    let incarnation_before = r.state().objects[&hojo].incarnation;
    assert_eq!(
        activate_on(&mut r, src, own, &[]),
        0,
        "the first qualifying activation"
    );
    cast_and_resolve(&mut r, blink, Some(hojo));
    let hojo_after = find_on_battlefield(&r, "Professor Hojo");
    assert!(
        hojo_after != hojo || r.state().objects[&hojo_after].incarnation != incarnation_before,
        "reach guard: Hojo is a new object (CR 400.7)"
    );
    assert_eq!(
        activate_on(&mut r, src, own, &[]),
        2,
        "not a second first activation"
    );
}

/// H2b: a second Hojo that enters after the turn's first qualifying activation
/// grants nothing: that activation was already the first.
#[test]
fn a_hojo_entering_after_the_first_qualifying_activation_grants_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Theirs", 1, 1);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let late = s
        .add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    mana(&mut s, P0, 10);
    let mut r = s.build();
    assert_eq!(
        activate_on(&mut r, src, own, &[]),
        0,
        "the first qualifying activation"
    );
    cast_and_resolve(&mut r, late, None);
    assert_eq!(
        r.state().objects[&late].zone,
        Zone::Battlefield,
        "reach guard: the second Hojo entered"
    );
    assert_eq!(
        activate_on(&mut r, src, own, &[]),
        2,
        "not a second first activation"
    );
}

/// H7: "targets a creature you control" is judged when the ability is
/// activated, before its cost is paid. P0's first activation targets the
/// creature P0 controls through Control Magic and sacrifices Control Magic as
/// its cost, so the creature reverts to P1 before the ability is placed. It
/// was still the first qualifying activation.
#[test]
fn a_target_that_changes_controller_while_its_cost_is_paid_still_qualified() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    let stolen = s.add_creature(P1, "Stolen", 2, 2).id();
    let magic = s
        .add_enchantment_from_oracle(
            P0,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let sacrificer = s
        .add_artifact_from_oracle(
            P0,
            "Altar of Change",
            "{1}, Sacrifice an enchantment: Tap target creature.",
        )
        .id();
    let hojo = s
        .add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    mana(&mut s, P0, 10);
    let mut r = s.build();
    {
        let state = r.state_mut();
        state.objects.get_mut(&magic).unwrap().attached_to = Some(stolen.into());
        state
            .objects
            .get_mut(&stolen)
            .unwrap()
            .attachments
            .push(magic);
        state.layers_dirty.mark_full();
    }
    engine::game::layers::flush_layers(r.state_mut());
    assert_eq!(
        r.state().objects[&stolen].controller,
        P0,
        "reach guard: P0 controls it"
    );
    activate_on(&mut r, sacrificer, stolen, &[magic]);
    assert_eq!(
        r.state().objects[&stolen].controller,
        P1,
        "reach guard: the creature reverted"
    );
    cast_and_resolve(&mut r, hojo, None);
    assert_eq!(
        activate_on(&mut r, src, own, &[]),
        2,
        "the first activation qualified"
    );
}

/// Pass priority until the stack is empty (both players pass each object).
fn drain(r: &mut GameRunner) {
    while !r.state().stack.is_empty() {
        act(r, GameAction::PassPriority).expect("pass");
    }
}

fn journal(r: &GameRunner, player: PlayerId) -> Vec<AbilityActivationRecord> {
    r.state()
        .abilities_activated_this_turn_by_player
        .get(&player)
        .map(|rows| rows.iter().cloned().collect())
        .unwrap_or_default()
}

/// H4: an earlier activation that doesn't target a creature you control (an
/// opponent's creature, or no target at all) is not the first QUALIFYING one,
/// whether it came before or after Hojo entered.
#[test]
fn a_non_qualifying_earlier_activation_does_not_consume_the_discount() {
    for (label, before_hojo) in [("with Hojo out", false), ("before Hojo", true)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        library(&mut s, P0);
        let own = s.add_creature(P0, "Own", 1, 1).id();
        let theirs = s.add_creature(P1, "Theirs", 1, 1).id();
        let hojo = if before_hojo {
            Some(
                s.add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
                    .with_mana_cost(ManaCost::generic(1))
                    .id(),
            )
        } else {
            s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
            None
        };
        let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
        let lifestone = s
            .add_artifact_from_oracle(P0, "Lifestone", "{2}: You gain 1 life.")
            .id();
        mana(&mut s, P0, 12);
        let mut r = s.build();
        assert_eq!(activate_on(&mut r, src, theirs, &[]), 2, "{label}: theirs");
        let before = pool(&r, P0);
        r.activate(lifestone, 0).resolve();
        assert_eq!(before - pool(&r, P0), 2, "{label}: untargeted");
        assert_eq!(
            journal(&r, P0).len(),
            2,
            "{label}: reach guard: both journaled"
        );
        if let Some(hojo) = hojo {
            cast_and_resolve(&mut r, hojo, None);
        }
        assert_eq!(
            activate_on(&mut r, src, own, &[]),
            0,
            "{label}: the first qualifying activation"
        );
    }
}

/// H5: an opponent's activation that targets your creature is not an ability
/// YOU activate, so it doesn't consume your Hojo's discount.
#[test]
fn an_opponents_activation_targeting_your_creature_does_not_consume_it() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    let theirs_src = s.add_artifact_from_oracle(P1, "Their Tapper", TAPPER).id();
    mana(&mut s, P0, 4);
    mana(&mut s, P1, 4);
    let mut r = s.build();
    act(&mut r, GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "reach guard: P1 has priority on P0's turn"
    );
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: theirs_src,
            ability_index: 0,
        },
    )
    .expect("P1 activates");
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        act(
            &mut r,
            GameAction::SelectTargets {
                targets: vec![object(own)],
            },
        )
        .expect("P1 targets P0's creature");
    }
    finish(&mut r, &[]);
    assert_eq!(pool(&r, P1), 2, "reach guard: P1 paid full");
    assert!(
        journal(&r, P1)
            .iter()
            .any(|row| row.activator == P1 && !row.targets.is_empty()),
        "reach guard: P1's activation is journaled under P1"
    );
    drain(&mut r);
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "back to P0 in the same main phase"
    );
    assert_eq!(
        activate_on(&mut r, src, own, &[]),
        0,
        "P0's first qualifying activation"
    );
}

/// H6: the journal keeps what was true when the ability was activated. The
/// first activation targeted P0's creature; P1 then gains control of it. It was
/// still the first qualifying activation.
#[test]
fn a_target_that_changes_controller_after_placement_still_qualified() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    let own2 = s.add_creature(P0, "Own Two", 1, 1).id();
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let steal = s
        .add_artifact_from_oracle(
            P1,
            "Steal Rod",
            "{1}: Gain control of target creature until end of turn.",
        )
        .id();
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    mana(&mut s, P0, 10);
    mana(&mut s, P1, 2);
    let mut r = s.build();
    assert_eq!(activate_on(&mut r, src, own, &[]), 0, "the first");
    act(&mut r, GameAction::PassPriority).expect("P0 passes");
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: steal,
            ability_index: 0,
        },
    )
    .expect("P1 steals");
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        act(
            &mut r,
            GameAction::SelectTargets {
                targets: vec![object(own)],
            },
        )
        .expect("P1 targets the first target");
    }
    finish(&mut r, &[]);
    drain(&mut r);
    assert_eq!(
        r.state().objects[&own].controller,
        P1,
        "reach guard: P1 now controls the first target"
    );
    assert_eq!(
        activate_on(&mut r, src, own2, &[]),
        2,
        "the first activation still qualified"
    );
}

const TAPPER_FOUR: &str = "{4}: Tap target creature.";

/// H8: two Hojos each apply their {2} to the SAME first qualifying activation,
/// and neither applies to the next, whichever entered first.
#[test]
fn two_hojos_both_discount_the_first_qualifying_activation_only() {
    for late_second in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        library(&mut s, P0);
        let own = s.add_creature(P0, "Own", 1, 1).id();
        s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
        let second = if late_second {
            Some(
                s.add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
                    .with_mana_cost(ManaCost::generic(1))
                    .id(),
            )
        } else {
            s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
            None
        };
        let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER_FOUR).id();
        mana(&mut s, P0, 10);
        let mut r = s.build();
        if let Some(second) = second {
            cast_and_resolve(&mut r, second, None);
        }
        let hojos = r
            .state()
            .objects
            .values()
            .filter(|o| o.name == "Professor Hojo" && o.zone == Zone::Battlefield)
            .count();
        assert_eq!(hojos, 2, "reach guard: two Hojos (late: {late_second})");
        assert_eq!(
            activate_on(&mut r, src, own, &[]),
            0,
            "both reduce the first (late: {late_second})"
        );
        assert_eq!(
            activate_on(&mut r, src, own, &[]),
            4,
            "neither reduces the second (late: {late_second})"
        );
    }
}

/// H9: the journal survives the engine's save/restore pipeline, and the
/// restored game still knows the turn's first qualifying activation happened.
#[test]
fn the_activation_journal_survives_save_and_restore() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    mana(&mut s, P0, 10);
    let mut r = s.build();
    assert_eq!(activate_on(&mut r, src, own, &[]), 0, "the first");
    let mut restored = round_trip(&r);
    assert_eq!(
        journal(&restored, P0),
        journal(&r, P0),
        "reach guard: the rows round-trip"
    );
    assert_eq!(
        activate_on(&mut restored, src, own, &[]),
        2,
        "the restored game still counts the first activation"
    );
}

// ---------------------------------------------------------------------------
// r5c #3: every viewer projection drops every activation record carrier, while
// the authoritative state keeps them.
// ---------------------------------------------------------------------------

fn projection_json(r: &GameRunner, viewer: PlayerId) -> String {
    serde_json::to_string(&filter_state_for_viewer(r.state(), viewer)).expect("projection")
}

fn assert_projections_carry_no_records(r: &GameRunner, viewers: &[PlayerId], when: &str) {
    for viewer in viewers {
        let json = projection_json(r, *viewer);
        assert!(
            !json.contains("activation_record")
                && !json.contains("abilities_activated_this_turn_by_player"),
            "{when}: viewer {viewer:?} sees an activation record"
        );
    }
}

fn sacrifice_tapper_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Theirs", 1, 1);
    let src = s
        .add_artifact_from_oracle(
            P0,
            "Altar",
            "{1}, Sacrifice an artifact: Tap target creature.",
        )
        .id();
    let fodder = s.add_artifact_from_oracle(P0, "Fodder", "").id();
    mana(&mut s, P0, 4);
    (s.build(), src, own, fodder)
}

#[test]
fn a_target_or_payment_prompt_projects_no_activation_record() {
    let (mut r, src, own, fodder) = sacrifice_tapper_board();
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("activation");
    let WaitingFor::TargetSelection { pending_cast, .. } = &r.state().waiting_for else {
        panic!(
            "expected the target prompt, got {:?}",
            r.state().waiting_for
        );
    };
    assert!(
        pending_cast.ability.activation_record.is_some(),
        "authoritative: the draft rides the target prompt"
    );
    assert!(
        serde_json::to_string(r.state())
            .unwrap()
            .contains("activation_record"),
        "reach guard: the authoritative state serializes the draft"
    );
    assert_projections_carry_no_records(&r, &[P0, P1], "target prompt");

    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(own)],
        },
    )
    .expect("target");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::PayCost { .. }),
        "reach guard: the sacrifice prompt, got {:?}",
        r.state().waiting_for
    );
    assert!(
        serde_json::to_string(r.state())
            .unwrap()
            .contains("activation_record"),
        "reach guard: the authoritative state still carries the draft at payment"
    );
    assert_projections_carry_no_records(&r, &[P0, P1], "payment prompt");
    finish(&mut r, &[fodder]);
    assert_eq!(journal(&r, P0).len(), 1, "the activation still completes");
}

/// r5c #2: placement never manufactures an activation's facts after its cost
/// is paid. An activation that reaches placement without its pre-payment draft
/// can't be completed legally, so it is reversed at the action boundary:
/// nothing is placed and no journal row is written. Every production route
/// carries the draft from a capture made before any payment, so this is
/// reachable only by a fixture that strips it.
#[test]
fn an_activation_that_lost_its_draft_is_reversed_not_recorded() {
    let (mut r, src, own, fodder) = sacrifice_tapper_board();
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("activation");
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(own)],
        },
    )
    .expect("target");
    let state = r.state_mut();
    let mut stripped = 0;
    if let Some(pending) = state.pending_cast.as_deref_mut() {
        stripped += usize::from(pending.ability.activation_record.take().is_some());
    }
    if let Some(pending) = state.waiting_for.pending_cast_mut() {
        stripped += usize::from(pending.ability.activation_record.take().is_some());
    }
    assert!(
        stripped > 0,
        "reach guard: the paused payment carried a draft"
    );
    let pool_before = pool(&r, P0);
    let result = r
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("the action is answered");
    assert!(
        !result.disposition.is_applied(),
        "a typed reversal: {:?}",
        result.waiting_for
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        r.state().waiting_for
    );
    assert_eq!(stack_len(&r), 0, "nothing placed");
    // The boundary restores the state from before THIS action: the sacrifice
    // it began is undone. (The mana leg was paid by the earlier target action,
    // which this unreachable-in-production strip happens after.)
    assert_eq!(pool(&r, P0), pool_before, "nothing more paid");
    assert_eq!(
        r.state().objects[&fodder].zone,
        Zone::Battlefield,
        "the sacrifice was not paid"
    );
    assert!(journal(&r, P0).is_empty(), "no journal row");
}

/// The journal holds non-mana activations only: a mana ability is counted
/// (CR 602.5b) but not journaled, whether the player activates it (here, one
/// that suspends on its color choice) or payment auto-taps it (a basic land's
/// intrinsic ability). The non-mana ability it paid for is journaled.
#[test]
fn mana_abilities_are_not_journaled_manual_or_automatic() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let prism = s
        .add_artifact_from_oracle(P0, "Prism", "{T}: Add one mana of any color.")
        .id();
    let forest = s.add_basic_land(P0, ManaColor::Green);
    let lifestone = s
        .add_artifact_from_oracle(P0, "Lifestone", "{2}: You gain 1 life.")
        .id();
    let mut r = s.build();

    // Manual.
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: prism,
            ability_index: 0,
        },
    )
    .expect("mana ability");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::ChooseManaColor { .. }),
        "reach guard: suspended on the color choice, got {:?}",
        r.state().waiting_for
    );
    assert_projections_carry_no_records(&r, &[P0, P1], "mana choice");
    act(
        &mut r,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        },
    )
    .expect("color");
    assert_eq!(
        r.state().activated_abilities_this_turn.get(&(prism, 0)),
        Some(&1),
        "reach guard: the manual mana ability completed and was counted"
    );
    assert!(journal(&r, P0).is_empty(), "manual: not journaled");

    // Automatic: the Prism's mana floats; payment auto-taps the Forest.
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: lifestone,
            ability_index: 0,
        },
    )
    .expect("activation");
    finish(&mut r, &[]);
    assert!(
        r.state().objects[&forest].tapped,
        "reach guard: payment auto-tapped the Forest"
    );
    let rows = journal(&r, P0);
    assert_eq!(
        rows.iter().map(|row| row.source).collect::<Vec<_>>(),
        vec![lifestone],
        "automatic: only the ability it paid for is journaled"
    );
}

#[test]
fn an_activation_on_the_stack_projects_no_activation_record() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_creature(P1, "Theirs", 1, 1);
    let src = s.add_artifact_from_oracle(P0, "Tapper", TAPPER).id();
    mana(&mut s, P0, 4);
    let mut r = s.build();
    act(
        &mut r,
        GameAction::ActivateAbility {
            source_id: src,
            ability_index: 0,
        },
    )
    .expect("activation");
    act(
        &mut r,
        GameAction::SelectTargets {
            targets: vec![object(own)],
        },
    )
    .expect("target");
    finish(&mut r, &[]);
    assert_eq!(stack_len(&r), 1, "reach guard: the ability is on the stack");
    assert_eq!(journal(&r, P0).len(), 1, "authoritative: journaled");
    assert_projections_carry_no_records(&r, &[P0, P1], "on the stack");
}

/// A source whose ability was activated and that then moved to a hidden zone:
/// no viewer of a three-player game, the activator included, gets its journal
/// row back. (Its name itself stays public knowledge through the bounce's own
/// last-known information; what is checked is that the journal is no channel.)
#[test]
fn a_journaled_source_that_moves_to_a_hidden_zone_is_not_exposed() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 11);
    s.at_phase(Phase::PreCombatMain);
    library(&mut s, P0);
    let own = s.add_creature(P0, "Own", 1, 1).id();
    let src = s.add_artifact_from_oracle(P0, "Secret Tapper", TAPPER).id();
    let bounce = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    mana(&mut s, P0, 6);
    let mut r = s.build();
    activate_on(&mut r, src, own, &[]);
    cast_and_resolve(&mut r, bounce, Some(src));
    assert_eq!(
        r.state().objects[&src].zone,
        Zone::Hand,
        "reach guard: the source is in a hidden zone"
    );
    assert_eq!(
        journal(&r, P0)[0].source_lki.name,
        "Secret Tapper",
        "authoritative: the row names its source"
    );
    assert_projections_carry_no_records(&r, &[P0, P1, p2], "hidden source");
}

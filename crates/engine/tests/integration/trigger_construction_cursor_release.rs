//! CR 603.3c + CR 603.3d: a triggered ability's mode, target and division
//! choices are made while it is put on the stack. When that construction ends,
//! every construction cursor — `pending_trigger`, `pending_trigger_firing`,
//! `pending_trigger_entry` and the `pending_trigger_event_batch` carrier — is
//! released together; from then on the live stack entry's own event row is the
//! event authority.
//!
//! These tests drive each completion seam (ChooseTarget, SelectTargets, modal
//! no-target, DistributeAmong) through `apply()` and assert the carrier is
//! released, plus the two downstream consumers a leaked carrier corrupted: the
//! proven-inert batch gate (`stack::priority_checkpoint_is_settled`) and a
//! sibling trigger's "that player" companion slot.

use std::collections::HashSet;

use engine::game::combat::AttackTarget;
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{AutoPassRequest, CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const SCUTE_SWARM_ORACLE: &str = "Landfall — Whenever a land you control enters, create a 1/1 green Insect creature token. If you control six or more lands, create a token that's a copy of this creature instead.";

const BRISTLY_BILL_ORACLE: &str = "Landfall — Whenever a land you control enters, put a +1/+1 counter on target creature.\n{3}{G}{G}: Double the number of +1/+1 counters on each creature you control.";

const FELIDAR_RETREAT_ORACLE: &str = "Landfall — Whenever a land you control enters, choose one —\n• Create a 2/2 white Cat Beast creature token.\n• Put a +1/+1 counter on each creature you control. Those creatures gain vigilance until end of turn.";

const INFERNO_TITAN_ORACLE: &str = "{R}: This creature gets +1/+0 until end of turn.\nWhenever this creature enters or attacks, it deals 3 damage divided as you choose among one, two, or three targets.";

const ALELA_ORACLE: &str = "Flying\nWhenever you cast your first spell during each opponent's turn, create a 1/1 black Faerie Rogue creature token with flying.\nWhenever one or more Faeries you control deal combat damage to a player, goad target creature that player controls.";

const SCUTE_COUNT: usize = 6;

struct LandfallBoard {
    runner: GameRunner,
    bristly: Option<ObjectId>,
}

/// P0 controls six Scute Swarm (and, optionally, Bristly Bill) with a Forest in
/// hand and no land on the battlefield, then plays the Forest. With a single
/// land each Scute trigger takes the Insect branch, so the Scute run is six
/// identical untargeted triggers.
fn landfall_board(with_bristly: bool) -> LandfallBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bristly = with_bristly.then(|| {
        scenario
            .add_creature_from_oracle(P0, "Bristly Bill, Spine Sower", 2, 2, BRISTLY_BILL_ORACLE)
            .with_subtypes(vec!["Plant", "Druid"])
            .as_legendary()
            .id()
    });
    for _ in 0..SCUTE_COUNT {
        scenario
            .add_creature_from_oracle(P0, "Scute Swarm", 1, 1, SCUTE_SWARM_ORACLE)
            .with_subtypes(vec!["Insect"]);
    }
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    LandfallBoard { runner, bristly }
}

/// CR 603.3b: P0 orders their own simultaneous landfall triggers, placing
/// Bristly Bill's first (bottom of the group), so it is constructed first and
/// the six Scute triggers wait behind it.
fn submit_bristly_first(runner: &mut GameRunner, bristly: ObjectId) {
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "seven landfall triggers must raise the CR 603.3b order prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(triggers.len(), SCUTE_COUNT + 1);
    let bristly_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == bristly)
        .expect("Bristly Bill's trigger is in the order prompt");
    let order = std::iter::once(bristly_index)
        .chain((0..triggers.len()).filter(|&index| index != bristly_index))
        .collect();
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("the engine-issued order is legal");
}

/// Submits the identity order for any CR 603.3b order prompt that is up.
fn submit_identity_order(runner: &mut GameRunner) {
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
    }
}

/// `pending_trigger_firing` is `pub(crate)`; the serialized state is the
/// integration-visible view. The paired positive control is
/// `firing_is_serialized`, asserted while construction is still paused.
fn serialized_firing(state: &GameState) -> serde_json::Value {
    serde_json::to_value(state)
        .expect("game state serializes")
        .get("pending_trigger_firing")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn firing_is_serialized(state: &GameState) -> bool {
    !serialized_firing(state).is_null()
}

fn firing_released(state: &GameState) -> bool {
    serialized_firing(state).is_null()
}

/// Reach guard shared by A1/A2/A5: Bristly Bill's trigger is paused on its
/// target prompt with every construction cursor live, and the six Scute
/// triggers are deferred behind it. Returns the first legal target.
fn assert_bristly_paused_mid_construction(runner: &GameRunner) -> TargetRef {
    let state = runner.state();
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &state.waiting_for else {
        panic!(
            "Bristly Bill's trigger must pause for its target, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        !state.pending_trigger_event_batch.is_empty(),
        "the paused trigger's carrier holds its landfall event"
    );
    assert!(state.pending_trigger_entry.is_some());
    assert!(state.pending_trigger.is_some());
    assert!(
        firing_is_serialized(state),
        "positive control: the serialized view sees a live firing"
    );
    assert_eq!(state.deferred_triggers.len(), SCUTE_COUNT);
    target_slots[0].legal_targets[0].clone()
}

/// The released-cursor assertions shared by every completion seam.
fn assert_construction_released(state: &GameState) {
    assert!(state.pending_trigger.is_none());
    assert!(state.pending_trigger_entry.is_none());
    assert!(firing_released(state));
    // Revert-failing: at base the completed trigger's carrier stays latched.
    assert!(
        state.pending_trigger_event_batch.is_empty(),
        "completed construction must release its event carrier, still holds {} event(s)",
        state.pending_trigger_event_batch.len()
    );
}

fn assert_bristly_entry_targets(runner: &GameRunner, bristly: ObjectId, chosen: &TargetRef) {
    let state = runner.state();
    assert_eq!(
        state.stack.len(),
        SCUTE_COUNT + 1,
        "Bristly Bill's entry and the six Scute triggers are on the stack"
    );
    let entry = state
        .stack
        .iter()
        .find(|entry| entry.source_id == bristly)
        .expect("Bristly Bill's entry is on the stack");
    assert_eq!(
        entry.ability().expect("a triggered ability").targets,
        vec![chosen.clone()]
    );
}

fn tokens_with_subtype(state: &GameState, subtype: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .count()
}

/// A1: CR 601.2c — the target is chosen through `ChooseTarget`; construction
/// completes and every cursor is released.
#[test]
fn choose_target_completion_releases_every_construction_cursor() {
    let LandfallBoard {
        mut runner,
        bristly,
    } = landfall_board(true);
    let bristly = bristly.expect("board has Bristly Bill");
    submit_bristly_first(&mut runner, bristly);
    let chosen = assert_bristly_paused_mid_construction(&runner);

    runner
        .act(GameAction::ChooseTarget {
            target: Some(chosen.clone()),
        })
        .expect("the engine-offered target is legal");

    assert_bristly_entry_targets(&runner, bristly, &chosen);
    assert_construction_released(runner.state());
}

/// A2: the same completion through `SelectTargets`.
#[test]
fn select_targets_completion_releases_every_construction_cursor() {
    let LandfallBoard {
        mut runner,
        bristly,
    } = landfall_board(true);
    let bristly = bristly.expect("board has Bristly Bill");
    submit_bristly_first(&mut runner, bristly);
    let chosen = assert_bristly_paused_mid_construction(&runner);

    runner
        .act(GameAction::SelectTargets {
            targets: vec![chosen.clone()],
        })
        .expect("the engine-offered target is legal");

    assert_bristly_entry_targets(&runner, bristly, &chosen);
    assert_construction_released(runner.state());
}

/// A3: CR 603.3c + CR 700.2b — a modal trigger's mode is chosen as it is put
/// on the stack; a mode with no target completes construction immediately.
#[test]
fn modal_no_target_completion_releases_cursors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Felidar Retreat", FELIDAR_RETREAT_ORACLE);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");

    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. }),
        "Felidar Retreat's landfall trigger pauses for its mode, got {:?}",
        state.waiting_for
    );
    assert!(!state.pending_trigger_event_batch.is_empty());
    assert!(state.pending_trigger_entry.is_some());

    runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("mode 0 is legal");

    assert_construction_released(runner.state());

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        tokens_with_subtype(runner.state(), "Cat"),
        1,
        "mode 0 creates exactly one Cat Beast"
    );
}

/// A4: CR 601.2d — the division is announced after the targets, so while
/// `DistributeAmong` is outstanding construction has not ended and the cursors
/// stay live; they are released only once the division is submitted.
#[test]
fn divided_trigger_releases_cursors_only_after_division() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let elf = scenario.add_creature(P1, "Elf", 1, 1).id();
    let titan = scenario
        .add_creature_to_hand_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_TITAN_ORACLE)
        .with_subtypes(vec!["Giant"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let mut runner = scenario.build();
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("P0 exists")
        .mana_pool;
    for _ in 0..8 {
        pool.add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }

    let card_id = runner.state().objects[&titan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: titan,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Inferno Titan is legal");
    for _ in 0..20 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.pass_both_players();
    }
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "Inferno Titan's ETB trigger pauses for targets, got {:?}",
            runner.state().waiting_for
        );
    };
    let chosen = vec![
        TargetRef::Object(bear),
        TargetRef::Object(elf),
        TargetRef::Player(P1),
    ];
    for target in &chosen {
        assert!(
            target_slots[0].legal_targets.contains(target),
            "{target:?} is offered by the engine"
        );
    }
    runner
        .act(GameAction::SelectTargets { targets: chosen })
        .expect("three distinct offered targets are legal");

    let WaitingFor::DistributeAmong { targets, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "three targets require a division, got {:?}",
            runner.state().waiting_for
        );
    };
    // Preservation: construction is still in progress mid-division.
    assert!(
        !runner.state().pending_trigger_event_batch.is_empty(),
        "the carrier stays live while the division is outstanding"
    );
    assert!(runner.state().pending_trigger_entry.is_some());

    runner
        .act(GameAction::DistributeAmong {
            distribution: targets.into_iter().map(|target| (target, 1)).collect(),
        })
        .expect("a 1/1/1 division is legal");

    assert_construction_released(runner.state());

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&bear].damage_marked, 1);
    assert_eq!(runner.state().objects[&elf].damage_marked, 1);
    assert_eq!(runner.life(P1), 19);
}

/// A5: once Bristly Bill's construction completes and releases its carrier,
/// the six identical Scute triggers above it are a proven-inert run, so a
/// single committed stack session batches them.
#[test]
fn targeted_trigger_then_identical_run_batches() {
    let LandfallBoard {
        mut runner,
        bristly,
    } = landfall_board(true);
    let bristly = bristly.expect("board has Bristly Bill");
    submit_bristly_first(&mut runner, bristly);
    let chosen = assert_bristly_paused_mid_construction(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(chosen.clone()),
        })
        .expect("the engine-offered target is legal");

    perf_counters::reset();
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("a committed stack session is legal");

    let state = runner.state();
    assert!(state.stack.is_empty());
    assert_eq!(tokens_with_subtype(state, "Insect"), SCUTE_COUNT);
    let TargetRef::Object(target) = chosen else {
        panic!("Bristly Bill targets a creature, got {chosen:?}");
    };
    assert_eq!(
        state.objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1)
    );
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64,
        "the six Scute triggers batch once the carrier is released"
    );
}

/// A5 control: without a targeted trigger in front, the identical run batches.
#[test]
fn identical_run_without_targeted_trigger_batches() {
    let LandfallBoard { mut runner, .. } = landfall_board(false);
    submit_identity_order(&mut runner);

    perf_counters::reset();
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("a committed stack session is legal");

    assert!(runner.state().stack.is_empty());
    assert_eq!(tokens_with_subtype(runner.state(), "Insect"), SCUTE_COUNT);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64
    );
}

/// A1-H: CR 603.2 + CR 603.3d + CR 601.2c — each Alela trigger fires from its
/// own combat-damage event, so "that player" is the player that trigger's
/// Faeries damaged. A completed sibling's carrier must never become the next
/// sibling's event authority.
#[test]
fn sibling_trigger_targets_from_its_own_event_not_a_completed_siblings_carrier() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let alela = scenario
        .add_creature(P0, "Alela, Cunning Conqueror", 2, 4)
        .from_oracle_text_with_keywords(&["Flying"], ALELA_ORACLE)
        .with_subtypes(vec!["Faerie", "Warlock"])
        .as_legendary()
        .id();
    let faerie_a = scenario
        .add_creature(P0, "Faerie A", 1, 1)
        .with_subtypes(vec!["Faerie"])
        .id();
    let faerie_b = scenario
        .add_creature(P0, "Faerie B", 1, 1)
        .with_subtypes(vec!["Faerie"])
        .id();
    let x1 = scenario.add_creature(P1, "x1", 2, 2).id();
    let x2 = scenario.add_creature(P2, "x2", 2, 2).id();
    let mut runner = scenario.build();

    let mut alela_entries = HashSet::new();
    let mut prompts: Vec<(Vec<Vec<TargetRef>>, bool)> = Vec::new();
    let mut attacked = false;
    for _ in 0..200 {
        alela_entries.extend(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| entry.source_id == alela)
                .map(|entry| entry.id),
        );
        if alela_entries.len() == 2 && runner.state().stack.is_empty() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                assert!(!attacked, "only one combat is expected");
                attacked = true;
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![
                            (faerie_a, AttackTarget::Player(P1)),
                            (faerie_b, AttackTarget::Player(P2)),
                        ],
                        bands: vec![],
                    })
                    .expect("both Faeries may attack");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declaring no blockers is legal");
            }
            WaitingFor::OrderTriggers { .. } => submit_identity_order(&mut runner),
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                let offered: Vec<Vec<TargetRef>> = target_slots
                    .iter()
                    .map(|slot| slot.legal_targets.clone())
                    .collect();
                prompts.push((
                    offered,
                    !runner.state().pending_trigger_event_batch.is_empty(),
                ));
                runner
                    .act(GameAction::SelectTargets {
                        targets: target_slots
                            .iter()
                            .map(|slot| slot.legal_targets[0].clone())
                            .collect(),
                    })
                    .expect("the engine-offered targets are legal");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("unexpected prompt in the Alela combat: {other:?}"),
        }
    }
    runner.advance_until_stack_empty();

    // Reach guards: both triggers reached the stack, at least one paused for
    // targets with its own carrier installed.
    assert!(attacked);
    assert_eq!(
        alela_entries.len(),
        2,
        "one Alela trigger per damaged player"
    );
    assert!(!prompts.is_empty(), "the first Alela trigger prompts");
    assert!(
        prompts[0].1,
        "the first prompt carries its own damage event"
    );
    // Negative sibling: the first trigger's companion slot is still bound to
    // its own damaged player (P1), offering only P1 and P1's creature.
    assert_eq!(
        prompts[0].0,
        vec![vec![TargetRef::Player(P1)], vec![TargetRef::Object(x1)]],
        "the first trigger's prompt offers only P1 and x1"
    );

    // Revert-failing: at base the second trigger read the first trigger's
    // leaked carrier and goaded x1 a second time, leaving x2 untouched.
    assert!(
        runner.state().objects[&x2].goaded_by.contains(&P0),
        "the trigger from damage to P2 goads P2's creature"
    );
    assert!(runner.state().objects[&x1].goaded_by.contains(&P0));
}

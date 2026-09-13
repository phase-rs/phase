//! Phase 3 — an announced target set on counter-placement recipients.
//!
//! CR 115.1d + CR 603.3d: "put … counter(s) on each of any number of target
//! <class>" makes a triggered ability targeted. Its controller announces the
//! target set as the trigger is put on the stack, and only the declared objects
//! get counters. CR 115.10a: an in-class object that was not declared is not
//! affected.
//!
//! Every card here is staged from its verbatim Oracle text (MTGJSON
//! `AtomicCards.json`):
//!
//! - Sweet-Gum Recluse: "Flash\nCascade\nReach\nWhen this creature enters, put
//!   three +1/+1 counters on each of any number of target creatures that
//!   entered this turn."
//! - Chong and Lily, Nomads: "Whenever one or more Bards you control attack,
//!   choose one —\n• Put a lore counter on each of any number of target Sagas
//!   you control.\n• Creatures you control get +1/+0 until end of turn for each
//!   lore counter among Sagas you control."
//!
//! GREEN-AT-BASE LABELLING (charter standard, `charter-frozen.md`): none of the
//! rows in this module is green at this phase's base. At base both promised
//! clauses are mass-classified `PutCounterAll` nodes with no announced target
//! set, so no `TriggerTargetSelection` prompt is raised and every row fails at
//! its drive helper. The snapshot gate is not cited: no `.snap` file names
//! either card.

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SWEET_GUM_RECLUSE_ORACLE: &str = "Flash\nCascade\nReach\nWhen this creature enters, put three +1/+1 counters on each of any number of target creatures that entered this turn.";

const CHONG_AND_LILY_ORACLE: &str = "Whenever one or more Bards you control attack, choose one —\n• Put a lore counter on each of any number of target Sagas you control.\n• Creatures you control get +1/+0 until end of turn for each lore counter among Sagas you control.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Pass priority until the stack is empty, returning every emitted event.
fn drain_stack_collecting_events(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        let result = runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance resolution");
        events.extend(result.events);
    }
    events
}

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

/// The union of every slot's legal targets, in first-seen order.
fn offered_union(slots: &[TargetSelectionSlot]) -> Vec<TargetRef> {
    let mut union = Vec::new();
    for target in slots.iter().flat_map(|slot| slot.legal_targets.iter()) {
        if !union.contains(target) {
            union.push(target.clone());
        }
    }
    union
}

/// A short, printable label for a `WaitingFor`, for the observed-sequence record.
fn waiting_label(waiting: &WaitingFor) -> String {
    format!("{waiting:?}").chars().take(96).collect()
}

fn has_put_counter_resolution(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutCounter,
                ..
            }
        )
    })
}

/// Sweet-Gum Recluse in P0's hand with exactly its mana, beside:
/// - `fresh_a`, `fresh_b`: P0 creatures that entered this turn (in class);
/// - `fresh_opp`: a P1 creature that entered this turn (in class, other controller);
/// - `old_bear`: a P0 creature that entered on a prior turn (out of class);
/// - `fresh_relic`: a P0 noncreature artifact that entered this turn (out of class).
///
/// Both libraries are empty, so Cascade exiles nothing.
struct RecluseBoard {
    runner: GameRunner,
    recluse: ObjectId,
    fresh_a: ObjectId,
    fresh_b: ObjectId,
    fresh_opp: ObjectId,
    old_bear: ObjectId,
    fresh_relic: ObjectId,
}

impl RecluseBoard {
    fn fixtures(&self) -> Vec<(ObjectId, &'static str)> {
        vec![
            (self.recluse, "recluse"),
            (self.fresh_a, "fresh_a"),
            (self.fresh_b, "fresh_b"),
            (self.fresh_opp, "fresh_opp"),
            (self.old_bear, "old_bear"),
            (self.fresh_relic, "fresh_relic"),
        ]
    }
}

fn recluse_board() -> RecluseBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fresh_a = scenario
        .add_creature(P0, "Fresh Bear A", 2, 2)
        .with_summoning_sickness()
        .id();
    let fresh_b = scenario
        .add_creature(P0, "Fresh Bear B", 2, 2)
        .with_summoning_sickness()
        .id();
    let old_bear = scenario.add_creature(P0, "Old Bear", 2, 2).id();
    let fresh_relic = scenario
        .add_artifact_from_oracle(P0, "Fresh Relic", "")
        .with_summoning_sickness()
        .id();
    let fresh_opp = scenario
        .add_creature(P1, "Fresh Opponent Bear", 2, 2)
        .with_summoning_sickness()
        .id();
    let recluse = scenario
        .add_creature_to_hand_from_oracle(P0, "Sweet-Gum Recluse", 0, 3, SWEET_GUM_RECLUSE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(6, ManaType::Green));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    RecluseBoard {
        runner,
        recluse,
        fresh_a,
        fresh_b,
        fresh_opp,
        old_bear,
        fresh_relic,
    }
}

/// CR 603.3d: cast the Recluse through the real pipeline, pass priority until it
/// resolves and enters, and stop at its ETB's stack-time target prompt. Panics
/// with the observed `WaitingFor` sequence if the prompt never appears.
fn cast_recluse_to_etb_target_prompt(board: &mut RecluseBoard) -> Vec<TargetSelectionSlot> {
    let recluse = board.recluse;
    board.runner.cast(recluse).commit();
    let mut observed = Vec::new();
    for _ in 0..40 {
        let waiting = board.runner.state().waiting_for.clone();
        observed.push(waiting_label(&waiting));
        match waiting {
            WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                ..
            } => {
                assert_eq!(
                    player, P0,
                    "the Recluse's controller chooses the ETB targets"
                );
                assert_eq!(
                    source_id,
                    Some(recluse),
                    "the target prompt must belong to the Recluse's ETB"
                );
                return target_slots;
            }
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => panic!(
                "no TriggerTargetSelection: the Recluse's ETB resolved without a stack-time \
                 target prompt; observed WaitingFor sequence {observed:#?}; P1P1 counters \
                 {:?}",
                board
                    .fixtures()
                    .iter()
                    .map(|(id, name)| (
                        *name,
                        counters(&board.runner, *id, CounterType::Plus1Plus1)
                    ))
                    .collect::<Vec<_>>()
            ),
            WaitingFor::Priority { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance the Recluse's resolution");
            }
            other => panic!(
                "unexpected WaitingFor before the Recluse's ETB target prompt: {other:?}; \
                 observed sequence {observed:#?}"
            ),
        }
    }
    panic!("the Recluse's cast did not reach a target prompt in 40 steps: {observed:#?}");
}

/// Row R1 (C-2 + C-4, Sweet-Gum Recluse). CR 603.3d + CR 115.1d: the ETB raises
/// a stack-time target prompt that offers the creatures that entered this turn
/// (any controller) and nothing outside that class. CR 115.10a: after declaring
/// `fresh_a` and `fresh_b`, exactly those two get three +1/+1 counters each; the
/// in-class but undeclared `fresh_opp` gets none, and neither do the
/// out-of-class `old_bear` and `fresh_relic` or the Recluse itself.
///
/// The Recluse entered this turn, so it is itself in class; its membership in
/// the offered set is recorded here, not asserted.
///
/// RED AT BASE (no `TriggerTargetSelection`: the base node is `PutCounterAll`).
/// Paired with `sweet_gum_recluse_with_zero_recipients_places_no_counters` on the
/// same board; MP-ARM-OFF and MP-RECOVER-OFF must each turn it red.
#[test]
fn sweet_gum_recluse_places_counters_on_each_declared_recipient_only() {
    let mut board = recluse_board();
    let slots = cast_recluse_to_etb_target_prompt(&mut board);
    let offered = offered_union(&slots);
    let fixtures = board.fixtures();

    for (id, name) in [
        (board.fresh_a, "fresh_a"),
        (board.fresh_b, "fresh_b"),
        (board.fresh_opp, "fresh_opp"),
    ] {
        assert!(
            offered.contains(&TargetRef::Object(id)),
            "reach: {name} (a creature that entered this turn) must be offered; offered \
             union {offered:?}; fixtures {fixtures:?}"
        );
    }
    for (id, name) in [
        (board.old_bear, "old_bear"),
        (board.fresh_relic, "fresh_relic"),
    ] {
        assert!(
            !offered.contains(&TargetRef::Object(id)),
            "C-4: {name} is outside the stated class and must be absent from the offered \
             set; offered union {offered:?}; fixtures {fixtures:?}"
        );
    }

    board
        .runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(board.fresh_a),
                TargetRef::Object(board.fresh_b),
            ],
        })
        .expect("declaring fresh_a and fresh_b must be accepted");
    let events = drain_stack_collecting_events(&mut board.runner);

    for (id, name) in [(board.fresh_a, "fresh_a"), (board.fresh_b, "fresh_b")] {
        assert_eq!(
            counters(&board.runner, id, CounterType::Plus1Plus1),
            3,
            "{name} was declared and must get exactly three +1/+1 counters"
        );
    }
    for (id, name) in [
        (board.fresh_opp, "fresh_opp"),
        (board.old_bear, "old_bear"),
        (board.fresh_relic, "fresh_relic"),
        (board.recluse, "recluse"),
    ] {
        assert_eq!(
            counters(&board.runner, id, CounterType::Plus1Plus1),
            0,
            "CR 115.10a: {name} was not declared and must get no counters"
        );
    }
    assert!(
        has_put_counter_resolution(&events),
        "the ETB's PutCounter must resolve, got {events:?}"
    );
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "the resolved ETB must return to priority, got {:?}",
        board.runner.state().waiting_for
    );
}

/// Row R3 (C-3, Sweet-Gum Recluse). CR 107.1c + CR 115.6: "any number" includes
/// zero, so the empty declaration is accepted, the ETB still resolves, and no
/// object on the board gets a counter.
///
/// RED AT BASE (no prompt). Paired positive:
/// `sweet_gum_recluse_places_counters_on_each_declared_recipient_only` on the
/// same board.
#[test]
fn sweet_gum_recluse_with_zero_recipients_places_no_counters() {
    let mut board = recluse_board();
    let slots = cast_recluse_to_etb_target_prompt(&mut board);
    let offered = offered_union(&slots);
    assert!(
        offered.contains(&TargetRef::Object(board.fresh_a)),
        "reach: the prompt must offer fresh_a; offered union {offered:?}; fixtures {:?}",
        board.fixtures()
    );

    board
        .runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("CR 115.6: the empty target declaration must be accepted");
    let events = drain_stack_collecting_events(&mut board.runner);

    assert!(
        has_put_counter_resolution(&events),
        "reach: the zero-target ETB must still resolve its PutCounter, got {events:?}"
    );
    for (id, name) in board.fixtures() {
        assert_eq!(
            counters(&board.runner, id, CounterType::Plus1Plus1),
            0,
            "no recipient was declared, so {name} must get no counters"
        );
    }
}

/// Chong and Lily (a Bard, so its own attack fires the trigger) on P0's
/// battlefield, attacking-eligible, beside three P0 Sagas, a P0 non-Saga
/// enchantment, and a P1 Saga. The Sagas have no chapter abilities, so the
/// CR 714.4 sacrifice does not apply to them.
struct ChongBoard {
    runner: GameRunner,
    chong: ObjectId,
    saga_a: ObjectId,
    saga_b: ObjectId,
    saga_c: ObjectId,
    saga_opp: ObjectId,
    plain_ench: ObjectId,
}

impl ChongBoard {
    fn fixtures(&self) -> Vec<(ObjectId, &'static str)> {
        vec![
            (self.chong, "chong"),
            (self.saga_a, "saga_a"),
            (self.saga_b, "saga_b"),
            (self.saga_c, "saga_c"),
            (self.saga_opp, "saga_opp"),
            (self.plain_ench, "plain_ench"),
        ]
    }
}

fn chong_board() -> ChongBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let chong = scenario
        .add_creature_from_oracle(P0, "Chong and Lily, Nomads", 3, 3, CHONG_AND_LILY_ORACLE)
        .with_subtypes(vec!["Human", "Bard", "Ally"])
        .as_legendary()
        .id();
    let saga_a = scenario
        .add_enchantment_from_oracle(P0, "Saga A", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let saga_b = scenario
        .add_enchantment_from_oracle(P0, "Saga B", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let saga_c = scenario
        .add_enchantment_from_oracle(P0, "Saga C", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let plain_ench = scenario
        .add_enchantment_from_oracle(P0, "Plain Enchantment", "")
        .id();
    let saga_opp = scenario
        .add_enchantment_from_oracle(P1, "Opponent Saga", "")
        .with_subtypes(vec!["Saga"])
        .id();
    let runner = scenario.build();
    ChongBoard {
        runner,
        chong,
        saga_a,
        saga_b,
        saga_c,
        saga_opp,
        plain_ench,
    }
}

/// CR 603.3c + CR 700.2b: attack with Chong, choose mode one when the trigger's
/// mode prompt appears, and stop at the mode's stack-time target prompt. Panics
/// with the observed `WaitingFor` sequence if the prompts do not arrive in that
/// order.
fn attack_with_chong_to_mode_one_target_prompt(board: &mut ChongBoard) -> Vec<TargetSelectionSlot> {
    let chong = board.chong;
    let runner = &mut board.runner;
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(chong, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Chong attacks P1");

    let mut observed = Vec::new();
    let mut chose_mode = false;
    for _ in 0..40 {
        let waiting = runner.state().waiting_for.clone();
        observed.push(waiting_label(&waiting));
        match waiting {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::AbilityModeChoice { .. } if !chose_mode => {
                runner
                    .act(GameAction::SelectModes { indices: vec![0] })
                    .expect("choosing mode one must succeed");
                chose_mode = true;
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } if chose_mode => {
                return target_slots;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => panic!(
                "no TriggerTargetSelection after SelectModes {{ [0] }} (mode chosen: \
                 {chose_mode}); observed WaitingFor sequence {observed:#?}"
            ),
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance Chong's trigger");
            }
            other => panic!(
                "unexpected WaitingFor on Chong's attack-trigger path (mode chosen: \
                 {chose_mode}): {other:?}; observed sequence {observed:#?}"
            ),
        }
    }
    panic!("Chong's attack trigger did not reach a target prompt in 40 steps: {observed:#?}");
}

/// Row R2 (C-2 + C-4, the modal member). CR 603.3c + CR 700.2b: the mode is
/// chosen as the trigger is put on the stack, then mode one's targets. The
/// prompt offers the Sagas P0 controls and nothing outside that class
/// (`saga_opp` is a Saga with the wrong controller; `plain_ench` is controlled
/// but not a Saga). CR 115.10a: after declaring `saga_a` and `saga_b`, exactly
/// those two get one lore counter each; mode two did not apply, so Chong's power
/// is unchanged. CR 714.4: the fixture Sagas have no chapter abilities, so none
/// is sacrificed.
///
/// RED AT BASE (no `TriggerTargetSelection` after `SelectModes`: the base mode
/// node is `PutCounterAll`). MP-ARM-OFF and MP-RECOVER-OFF must each turn it red.
#[test]
fn chong_and_lily_mode_one_places_lore_on_each_declared_saga_only() {
    let mut board = chong_board();
    let fixtures = board.fixtures();
    let lore0: Vec<(ObjectId, &'static str, u32)> = fixtures
        .iter()
        .map(|(id, name)| (*id, *name, counters(&board.runner, *id, CounterType::Lore)))
        .collect();

    let slots = attack_with_chong_to_mode_one_target_prompt(&mut board);
    let offered = offered_union(&slots);

    for (id, name) in [
        (board.saga_a, "saga_a"),
        (board.saga_b, "saga_b"),
        (board.saga_c, "saga_c"),
    ] {
        assert!(
            offered.contains(&TargetRef::Object(id)),
            "reach: {name} (a Saga you control) must be offered; offered union {offered:?}; \
             fixtures {fixtures:?}"
        );
    }
    for (id, name) in [
        (board.saga_opp, "saga_opp"),
        (board.plain_ench, "plain_ench"),
        (board.chong, "chong"),
    ] {
        assert!(
            !offered.contains(&TargetRef::Object(id)),
            "C-4: {name} is outside the stated class and must be absent from the offered \
             set; offered union {offered:?}; fixtures {fixtures:?}"
        );
    }

    board
        .runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(board.saga_a),
                TargetRef::Object(board.saga_b),
            ],
        })
        .expect("declaring saga_a and saga_b must be accepted");
    let events = drain_stack_collecting_events(&mut board.runner);

    for (id, name, before) in &lore0 {
        let expected = if *id == board.saga_a || *id == board.saga_b {
            before + 1
        } else {
            *before
        };
        assert_eq!(
            counters(&board.runner, *id, CounterType::Lore),
            expected,
            "CR 115.10a: {name} lore must go from {before} to {expected} (declared: saga_a, \
             saga_b)"
        );
    }
    assert_eq!(
        board.runner.state().objects[&board.chong].power,
        Some(3),
        "mode two did not apply, so Chong's power must be unchanged"
    );
    assert!(
        has_put_counter_resolution(&events),
        "mode one's PutCounter must resolve, got {events:?}"
    );
    for (id, name) in [
        (board.saga_a, "saga_a"),
        (board.saga_b, "saga_b"),
        (board.saga_c, "saga_c"),
        (board.saga_opp, "saga_opp"),
    ] {
        assert_eq!(
            board.runner.state().objects[&id].zone,
            Zone::Battlefield,
            "reach: {name} has no chapter abilities and must stay on the battlefield"
        );
    }
}

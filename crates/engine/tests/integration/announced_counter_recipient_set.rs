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

use engine::game::ability_utils::{assign_selected_slots_in_chain, build_target_slots};
use engine::game::combat::has_cant_be_blocked_static;
use engine::game::keywords::has_keyword;
use engine::types::ability::{
    ContinuousModification, ControllerRef, Effect, MultiTargetSpec, QuantityExpr, ResolvedAbility,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::statics::StaticMode;

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

// ══ Phase 4 — U1 / U3 / U4 rows (run "any-number") ═══════════════════════════
//
// Every card below is staged from its verbatim Oracle text (MTGJSON
// `AtomicCards.json`, first face, reminder text stripped):
//
// - Filigree Vector: "When this creature enters, put a +1/+1 counter on each of
//   any number of target creatures and a charge counter on each of any number of
//   target artifacts.\n{1}, {T}, Sacrifice another artifact: Proliferate."
// - River Heralds' Boon: "Put a +1/+1 counter on target creature and a +1/+1
//   counter on up to one target Merfolk."
// - Drillworks Mole: "{2}, {T}: Put a +1/+1 counter on this creature and a +1/+1
//   counter on up to one target commander creature you control."
// - Trygon Prime: "Subterranean Assault — Whenever this creature attacks, put a
//   +1/+1 counter on it and a +1/+1 counter on up to one other target attacking
//   creature. That creature can't be blocked this turn."
// - Explosive Entry: "Destroy up to one target artifact. Put a +1/+1 counter on
//   up to one target creature."

const FILIGREE_VECTOR_ORACLE: &str = "When this creature enters, put a +1/+1 counter on each of any number of target creatures and a charge counter on each of any number of target artifacts.";

const RIVER_HERALDS_BOON_ORACLE: &str =
    "Put a +1/+1 counter on target creature and a +1/+1 counter on up to one target Merfolk.";

const DRILLWORKS_MOLE_ORACLE: &str = "{2}, {T}: Put a +1/+1 counter on this creature and a +1/+1 counter on up to one target commander creature you control.";

const TRYGON_PRIME_ORACLE: &str = "Subterranean Assault — Whenever this creature attacks, put a +1/+1 counter on it and a +1/+1 counter on up to one other target attacking creature. That creature can't be blocked this turn.";

const EXPLOSIVE_ENTRY_ORACLE: &str =
    "Destroy up to one target artifact. Put a +1/+1 counter on up to one target creature.";

fn charge_counter() -> CounterType {
    CounterType::Generic("charge".to_string())
}

/// True while the runner is waiting for a slot-by-slot target declaration.
fn in_target_selection(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    )
}

/// The legal targets of the slot the runner is currently asking about.
fn current_slot_legal_targets(runner: &GameRunner) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots
            .get(selection.current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Drive to the next target prompt, returning its declared slot group. Orders
/// triggers and passes priority on the way, and panics with the observed
/// `WaitingFor` sequence when no prompt arrives.
fn to_target_prompt(runner: &mut GameRunner, what: &str) -> Vec<TargetSelectionSlot> {
    let mut observed = Vec::new();
    for _ in 0..40 {
        let waiting = runner.state().waiting_for.clone();
        observed.push(waiting_label(&waiting));
        match waiting {
            WaitingFor::TriggerTargetSelection { target_slots, .. }
            | WaitingFor::TargetSelection { target_slots, .. } => return target_slots,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => panic!(
                "{what}: no target prompt — the stack is already empty at priority; \
                 observed {observed:#?}"
            ),
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance the prompt");
            }
            other => panic!("{what}: unexpected {other:?}; observed {observed:#?}"),
        }
    }
    panic!("{what}: no target prompt in 40 steps; observed {observed:#?}");
}

/// Declare `wants` slot by slot: each slot takes the next wanted target when it
/// is legal there, and is declined otherwise (CR 115.6 — declining an optional
/// slot is a legal declaration). The per-slot results are returned so a row can
/// assert that the declaration was accepted at all.
fn declare_slot_by_slot(runner: &mut GameRunner, wants: &[TargetRef]) -> Vec<Result<(), String>> {
    let mut outcomes: Vec<Result<(), String>> = Vec::new();
    let mut next = 0;
    while in_target_selection(runner) && outcomes.len() < 20 {
        let legal = current_slot_legal_targets(runner);
        let pick = wants.get(next).filter(|want| legal.contains(want)).cloned();
        if pick.is_some() {
            next += 1;
        }
        let result = runner.act(GameAction::ChooseTarget { target: pick });
        let failed = result.is_err();
        outcomes.push(result.map(|_| ()).map_err(|err| format!("{err:?}")));
        if failed {
            break;
        }
    }
    outcomes
}

/// The first rejected slot declaration, for a row's failure message.
fn first_declaration_error(outcomes: &[Result<(), String>]) -> Option<String> {
    outcomes
        .iter()
        .find_map(|outcome| outcome.as_ref().err().cloned())
}

/// Every object a transient continuous effect names directly, in effect order.
/// A grant bound to a declared target stores that target as `SpecificObject`,
/// so this is the list of objects phase 4's binding produced.
fn tce_recipients(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter_map(|tce| match tce.affected {
            TargetFilter::SpecificObject { id } => Some(id),
            _ => None,
        })
        .collect()
}

/// Every transient continuous effect's affected filter, for rows that must show
/// no grant of any shape was installed.
fn tce_affected(runner: &GameRunner) -> Vec<TargetFilter> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .map(|tce| tce.affected.clone())
        .collect()
}

fn put_counter_resolutions(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutCounter,
                    ..
                }
            )
        })
        .count()
}

struct FiligreeBoard {
    runner: GameRunner,
    fv: ObjectId,
    a: ObjectId,
    b: ObjectId,
    c: ObjectId,
    opp: ObjectId,
    r1: ObjectId,
    r2: ObjectId,
    r3: ObjectId,
}

/// Filigree Vector cast from P0's hand, with its ETB trigger about to ask for
/// targets. Filigree Vector is itself an Artifact Creature, so it is a legal
/// "target artifact" of its own second conjunct.
fn filigree_board() -> FiligreeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    let opp = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let r1 = scenario.add_artifact_from_oracle(P0, "Relic 1", "").id();
    let r2 = scenario.add_artifact_from_oracle(P0, "Relic 2", "").id();
    let r3 = scenario.add_artifact_from_oracle(P0, "Relic 3", "").id();
    let fv = scenario
        .add_creature_to_hand_from_oracle(P0, "Filigree Vector", 1, 1, FILIGREE_VECTOR_ORACLE)
        .as_artifact()
        .as_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::White));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(fv).commit();
    FiligreeBoard {
        runner,
        fv,
        a,
        b,
        c,
        opp,
        r1,
        r2,
        r3,
    }
}

/// D-2. CR 601.2c + CR 115.1: each instance of "target" in Filigree Vector's
/// ETB trigger announces its own set, so the declared creatures take the +1/+1
/// counters and the declared artifacts take the charge counters. CR 107.1c +
/// CR 115.6: "each of any number of target" includes zero, so every slot of the
/// artifact group is optional. CR 115.10a: an in-class object that was not
/// declared (c, opp, r3, and Filigree Vector itself) is not affected.
///
/// RED AT BASE: the artifact group's slots are announced as required
/// (`optional=false`), and only the first declared artifact is reached.
///
/// D-8 note (observed, not intended — this is what the flat `SelectTargets`
/// path does on this same card after phase 4, not a promise about it):
/// `[a, b]` places a +1/+1 counter on `a` and a charge counter on `b`
/// (unchanged from base); `[a, r1]` is refused (unchanged); the whole creature
/// group plus one artifact is assigned correctly (unchanged); the whole
/// creature group plus two artifacts is refused at base and is accepted after
/// phase 4 with the first artifact swallowed into the creature node; and `[]`
/// is refused at base and accepted after phase 4 with nothing placed. The flat
/// path executes none of the slot-path window code this phase changes. Of the
/// 54 Random-mode chains printed across 52 cards, none has the declared
/// swallow shape (RANDOM-HAZARD-DECLARED chains=0).
#[test]
fn filigree_vector_declares_many_creatures_and_many_artifacts_slot_by_slot() {
    let mut board = filigree_board();
    let slots = to_target_prompt(&mut board.runner, "Filigree Vector ETB");
    let artifact_slots: Vec<&TargetSelectionSlot> = slots
        .iter()
        .filter(|slot| slot.legal_targets.contains(&TargetRef::Object(board.r1)))
        .collect();
    assert!(
        !artifact_slots.is_empty(),
        "reach guard: the prompt must offer an artifact slot; slots {slots:#?}"
    );
    for (index, slot) in artifact_slots.iter().enumerate() {
        assert!(
            slot.optional,
            "CR 107.1c + CR 115.6: artifact slot {index} must be optional, got {slot:#?}"
        );
    }
    for slot in &artifact_slots {
        for creature_only in [board.a, board.b, board.c, board.opp] {
            assert!(
                !slot
                    .legal_targets
                    .contains(&TargetRef::Object(creature_only)),
                "the artifact group's legal set must hold no creature-only object"
            );
        }
    }

    let wants: Vec<TargetRef> = [board.a, board.b, board.r1, board.r2]
        .into_iter()
        .map(TargetRef::Object)
        .collect();
    let outcomes = declare_slot_by_slot(&mut board.runner, &wants);
    assert_eq!(
        first_declaration_error(&outcomes),
        None,
        "every slot declaration must be accepted"
    );

    let events = drain_stack_collecting_events(&mut board.runner);
    assert_eq!(
        put_counter_resolutions(&events),
        2,
        "both counter instructions must resolve"
    );
    for declared in [board.a, board.b] {
        assert_eq!(
            counters(&board.runner, declared, CounterType::Plus1Plus1),
            1,
            "a declared creature takes exactly one +1/+1 counter"
        );
    }
    for declared in [board.r1, board.r2] {
        assert_eq!(
            counters(&board.runner, declared, charge_counter()),
            1,
            "a declared artifact takes exactly one charge counter"
        );
    }
    for undeclared in [board.c, board.opp, board.r3, board.fv] {
        assert_eq!(
            counters(&board.runner, undeclared, CounterType::Plus1Plus1),
            0,
            "CR 115.10a: an undeclared object gets no +1/+1 counter"
        );
        assert_eq!(
            counters(&board.runner, undeclared, charge_counter()),
            0,
            "CR 115.10a: an undeclared object gets no charge counter"
        );
    }
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "the run must end at priority, got {:?}",
        board.runner.state().waiting_for
    );
}

/// D-3a. CR 107.1c + CR 115.6: the artifact group may legally be declared
/// empty, and the creature group still places its counters.
///
/// RED AT BASE: skipping the artifact slot is rejected with
/// `Cannot skip a required target`.
///
/// Positive reach guard:
/// `filigree_vector_declares_many_creatures_and_many_artifacts_slot_by_slot`
/// declares artifacts on the identical board, so "no charge counter anywhere"
/// here is a real decline and not a fixture that never reached the prompt.
#[test]
fn filigree_vector_declares_creatures_and_zero_artifacts() {
    let mut board = filigree_board();
    to_target_prompt(&mut board.runner, "Filigree Vector ETB");
    let wants: Vec<TargetRef> = [board.a, board.b]
        .into_iter()
        .map(TargetRef::Object)
        .collect();
    let outcomes = declare_slot_by_slot(&mut board.runner, &wants);
    assert_eq!(
        first_declaration_error(&outcomes),
        None,
        "declining every artifact slot must be accepted"
    );

    let events = drain_stack_collecting_events(&mut board.runner);
    assert_eq!(put_counter_resolutions(&events), 2);
    for declared in [board.a, board.b] {
        assert_eq!(
            counters(&board.runner, declared, CounterType::Plus1Plus1),
            1,
            "the declared creatures still take their counters"
        );
    }
    for object in [
        board.fv, board.a, board.b, board.c, board.opp, board.r1, board.r2, board.r3,
    ] {
        assert_eq!(
            counters(&board.runner, object, charge_counter()),
            0,
            "no object may take a charge counter when the artifact group is empty"
        );
    }
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "the run must end at priority, got {:?}",
        board.runner.state().waiting_for
    );
}

/// D-3b. CR 107.1c + CR 115.6: both announced sets may be declared empty; the
/// ability still resolves and places nothing.
///
/// RED AT BASE: skipping the artifact slot is rejected with
/// `Cannot skip a required target`.
///
/// Positive reach guard:
/// `filigree_vector_declares_many_creatures_and_many_artifacts_slot_by_slot`.
#[test]
fn filigree_vector_all_zero_declaration_places_nothing() {
    let mut board = filigree_board();
    to_target_prompt(&mut board.runner, "Filigree Vector ETB");
    let outcomes = declare_slot_by_slot(&mut board.runner, &[]);
    assert_eq!(
        first_declaration_error(&outcomes),
        None,
        "an all-zero declaration must be accepted"
    );

    drain_stack_collecting_events(&mut board.runner);
    for object in [
        board.fv, board.a, board.b, board.c, board.opp, board.r1, board.r2, board.r3,
    ] {
        assert_eq!(
            counters(&board.runner, object, CounterType::Plus1Plus1),
            0,
            "an all-zero declaration places no +1/+1 counter"
        );
        assert_eq!(
            counters(&board.runner, object, charge_counter()),
            0,
            "an all-zero declaration places no charge counter"
        );
    }
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "the run must end at priority, got {:?}",
        board.runner.state().waiting_for
    );
}

struct BoonBoard {
    runner: GameRunner,
    boon: ObjectId,
    bear: ObjectId,
    m1: ObjectId,
    m2: ObjectId,
}

/// River Heralds' Boon in P0's hand with its mana, a plain Bear and two Merfolk
/// on the battlefield.
fn boon_board() -> BoonBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let m1 = scenario
        .add_creature(P0, "Merfolk One", 1, 1)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let m2 = scenario
        .add_creature(P0, "Merfolk Two", 1, 1)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let boon = scenario
        .add_spell_to_hand_from_oracle(P0, "River Heralds' Boon", true, RIVER_HERALDS_BOON_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Green));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    BoonBoard {
        runner,
        boon,
        bear,
        m1,
        m2,
    }
}

/// D-4 (River Heralds' Boon, declined). CR 115.6: "up to one target Merfolk"
/// may be declared empty, and the mandatory first instance still places its
/// counter.
///
/// RED AT BASE: the sub conjunct's slot is announced as required, so the cast
/// panics with "could not satisfy required target slot 1".
///
/// Positive reach guard: `river_heralds_boon_declared_merfolk_gets_its_counter`
/// on the identical board declares the Merfolk and does move a counter.
#[test]
fn river_heralds_boon_declined_merfolk_slot_still_counters_the_creature() {
    let mut board = boon_board();
    let boon = board.boon;
    let outcome = board
        .runner
        .cast(boon)
        .target_objects(&[board.bear])
        .resolve();

    assert_eq!(
        counters(&board.runner, board.bear, CounterType::Plus1Plus1),
        1,
        "the declared creature takes exactly one +1/+1 counter"
    );
    for merfolk in [board.m1, board.m2] {
        assert_eq!(
            counters(&board.runner, merfolk, CounterType::Plus1Plus1),
            0,
            "CR 115.6: a declined optional target takes nothing"
        );
    }
    assert_eq!(
        outcome.zone_of(boon),
        Zone::Graveyard,
        "reach guard: the spell resolved and went to the graveyard"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// D-4 (River Heralds' Boon, declared). GREEN AT BASE — this row is the
/// positive reach guard of
/// `river_heralds_boon_declined_merfolk_slot_still_counters_the_creature`,
/// whose own pairing is its red-at-base reading; its pairing is
/// MP-P4-RECOVERY-OFF.
#[test]
fn river_heralds_boon_declared_merfolk_gets_its_counter() {
    let mut board = boon_board();
    let boon = board.boon;
    board
        .runner
        .cast(boon)
        .target_objects(&[board.bear, board.m1])
        .resolve();

    assert_eq!(
        counters(&board.runner, board.bear, CounterType::Plus1Plus1),
        1,
        "the mandatory instance's declared creature takes its counter"
    );
    assert_eq!(
        counters(&board.runner, board.m1, CounterType::Plus1Plus1),
        1,
        "CR 601.2c: the second instance's declared Merfolk takes its own counter"
    );
    assert_eq!(
        counters(&board.runner, board.m2, CounterType::Plus1Plus1),
        0,
        "CR 115.10a: the undeclared Merfolk takes nothing"
    );
}

struct MoleBoard {
    runner: GameRunner,
    mole: ObjectId,
    cmdr: ObjectId,
    plain: ObjectId,
    opp_cmdr: ObjectId,
}

/// Drillworks Mole on P0's battlefield with two generic mana floating, beside
/// P0's commander, a plain P0 creature and P1's commander.
fn mole_board() -> MoleBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mole = scenario
        .add_creature_from_oracle(P0, "Drillworks Mole", 1, 1, DRILLWORKS_MOLE_ORACLE)
        .as_artifact()
        .as_creature()
        .id();
    let cmdr = scenario
        .add_creature(P0, "Commander Bear", 2, 2)
        .commander()
        .id();
    let plain = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let opp_cmdr = scenario
        .add_creature(P1, "Opp Commander", 2, 2)
        .commander()
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    MoleBoard {
        runner,
        mole,
        cmdr,
        plain,
        opp_cmdr,
    }
}

/// D-4 (Drillworks Mole, declined). CR 115.6: "up to one target commander
/// creature you control" may be declared empty, and the activated ability's
/// first instance still places its counter on the Mole.
///
/// RED AT BASE: the sub conjunct's single slot is required, so it is
/// auto-declared and the commander takes a counter nobody announced.
///
/// Positive reach guard: `drillworks_mole_declared_commander_gets_its_counter`.
#[test]
fn drillworks_mole_declined_commander_slot_counters_only_the_mole() {
    let mut board = mole_board();
    let mole = board.mole;
    board.runner.activate(mole, 0).target_objects(&[]).resolve();

    assert_eq!(
        counters(&board.runner, mole, CounterType::Plus1Plus1),
        1,
        "the Mole takes exactly one +1/+1 counter"
    );
    for undeclared in [board.cmdr, board.plain, board.opp_cmdr] {
        assert_eq!(
            counters(&board.runner, undeclared, CounterType::Plus1Plus1),
            0,
            "CR 115.6 + CR 115.10a: nothing else may take a counter"
        );
    }
}

/// D-4 (Drillworks Mole, declared). GREEN AT BASE on every conjunct except the
/// legal-set conjunct — it is the positive reach guard of
/// `drillworks_mole_declined_commander_slot_counters_only_the_mole`. The
/// legal-set conjunct is NOT observable at base: with no recovered announced set
/// the single required slot is auto-declared and no prompt is raised, so it is
/// asserted on the final candidate only. Its pairing is MP-P4-RECOVERY-OFF,
/// under which the activation returns to priority with no target slots at all.
#[test]
fn drillworks_mole_declared_commander_gets_its_counter() {
    let mut board = mole_board();
    let mole = board.mole;
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: mole,
            ability_index: 0,
        })
        .expect("activating the Mole must be legal");
    // CR 601.2c + CR 115.6: the conjunct's own announced set makes the slot a
    // real announced choice, so its legal set is observable here. The explicit
    // drive is deliberate: the builder form consumes the prompt and cannot
    // observe it.
    let slots = match &board.runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots.clone(),
        other => panic!("expected TargetSelection, got {other:?}"),
    };
    assert_eq!(
        slots.len(),
        1,
        "the recovered announced set raises exactly one slot"
    );
    assert!(
        slots[0].optional,
        "CR 107.1c + CR 115.6: the recovered slot is optional"
    );
    assert!(
        slots[0]
            .legal_targets
            .contains(&TargetRef::Object(board.cmdr)),
        "the controller's own commander creature is in the legal set"
    );
    assert!(
        !slots[0]
            .legal_targets
            .contains(&TargetRef::Object(board.opp_cmdr)),
        "CR 115.1: an opponent's commander does not match the announced \
         filter, so it is absent from the legal set"
    );
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.cmdr)),
        })
        .expect("declaring the commander must be accepted");
    drain_stack_collecting_events(&mut board.runner);

    assert_eq!(
        counters(&board.runner, mole, CounterType::Plus1Plus1),
        1,
        "the Mole takes its own counter"
    );
    assert_eq!(
        counters(&board.runner, board.cmdr, CounterType::Plus1Plus1),
        1,
        "CR 601.2c: the declared commander creature takes the second counter"
    );
    for undeclared in [board.plain, board.opp_cmdr] {
        assert_eq!(
            counters(&board.runner, undeclared, CounterType::Plus1Plus1),
            0,
            "CR 115.10a: an undeclared object takes nothing"
        );
    }
}

struct TrygonBoard {
    runner: GameRunner,
    trygon: ObjectId,
    bear1: ObjectId,
    bear2: ObjectId,
}

/// Trygon Prime and two Bears attacking P1, with the attack trigger on the
/// stack.
fn trygon_board() -> TrygonBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let trygon = scenario
        .add_creature_from_oracle(P0, "Trygon Prime", 4, 4, TRYGON_PRIME_ORACLE)
        .with_subtypes(vec!["Tyranid"])
        .id();
    let bear1 = scenario.add_creature(P0, "Attacking Bear One", 2, 2).id();
    let bear2 = scenario.add_creature(P0, "Attacking Bear Two", 2, 2).id();
    scenario.add_creature(P1, "Opp Blocker", 2, 2);
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (trygon, AttackTarget::Player(P1)),
                (bear1, AttackTarget::Player(P1)),
                (bear2, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .expect("the three creatures attack P1");
    TrygonBoard {
        runner,
        trygon,
        bear1,
        bear2,
    }
}

/// D-4 (Trygon Prime, declared) + D-18. CR 608.2c: "That creature can't be
/// blocked this turn" is a separate sentence whose referent is the declared
/// target of the preceding instance, not the trigger's source. CR 509.1b: the
/// restriction lands on that declared creature only.
///
/// RED AT BASE: the grant registers on Trygon Prime.
#[test]
fn trygon_prime_declared_sub_target_is_the_creature_that_cant_be_blocked() {
    let mut board = trygon_board();
    to_target_prompt(&mut board.runner, "Trygon Prime attack trigger");
    let outcomes = declare_slot_by_slot(&mut board.runner, &[TargetRef::Object(board.bear1)]);
    assert_eq!(
        first_declaration_error(&outcomes),
        None,
        "declaring the sub target must be accepted"
    );
    drain_stack_collecting_events(&mut board.runner);

    assert_eq!(
        counters(&board.runner, board.trygon, CounterType::Plus1Plus1),
        1,
        "the head instruction puts a counter on the source"
    );
    assert_eq!(
        counters(&board.runner, board.bear1, CounterType::Plus1Plus1),
        1,
        "the declared attacking creature takes the second counter"
    );
    assert!(
        has_cant_be_blocked_static(board.runner.state(), board.bear1),
        "CR 608.2c + CR 509.1b: the declared creature is the one that can't be blocked"
    );
    for other in [board.trygon, board.bear2] {
        assert!(
            !has_cant_be_blocked_static(board.runner.state(), other),
            "no other attacker may gain the restriction"
        );
    }
}

/// D-4 (Trygon Prime, declined) + D-18. CR 115.6: the optional sub target may
/// be declared empty. CR 608.2c: with nothing declared, "That creature" names
/// nothing and no creature gains the restriction — in particular not the
/// source. CR 608.2b does not govern this: a target that was never chosen is
/// not a target that became illegal, so the ability still resolves.
///
/// RED AT BASE: the slot is required, so the decline is rejected.
#[test]
fn trygon_prime_declined_sub_target_grants_nothing() {
    let mut board = trygon_board();
    to_target_prompt(&mut board.runner, "Trygon Prime attack trigger");
    let outcomes = declare_slot_by_slot(&mut board.runner, &[]);
    assert_eq!(
        first_declaration_error(&outcomes),
        None,
        "declining the optional sub target must be accepted"
    );
    drain_stack_collecting_events(&mut board.runner);

    assert_eq!(
        counters(&board.runner, board.trygon, CounterType::Plus1Plus1),
        1,
        "the head instruction still puts its counter on the source"
    );
    for other in [board.bear1, board.bear2] {
        assert_eq!(
            counters(&board.runner, other, CounterType::Plus1Plus1),
            0,
            "no other object may take a counter when the sub target was declined"
        );
    }
    for object in [board.trygon, board.bear1, board.bear2] {
        assert!(
            !has_cant_be_blocked_static(board.runner.state(), object),
            "CR 115.6 + CR 608.2c: a declined target grants the restriction to nobody"
        );
    }
}

/// D-9 (ratification). CR 107.1c + CR 115.6: mode one's "each of any number of
/// target Sagas you control" may be declared empty; the mode still resolves and
/// places no lore counter.
///
/// GREEN AT BASE — labelled. Its paired positive is
/// `chong_and_lily_mode_one_places_lore_on_each_declared_saga_only` (the same
/// fixture with Sagas declared), and its pairing is MP-RECOVER-OFF.
#[test]
fn chong_and_lily_mode_one_with_zero_sagas_places_no_lore() {
    let mut board = chong_board();
    attack_with_chong_to_mode_one_target_prompt(&mut board);
    board
        .runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("CR 115.6: an empty announced set must be accepted");

    let events = drain_stack_collecting_events(&mut board.runner);
    assert_eq!(
        put_counter_resolutions(&events),
        1,
        "the mode must still resolve exactly once"
    );
    for (id, label) in board.fixtures() {
        assert_eq!(
            counters(&board.runner, id, CounterType::Lore),
            0,
            "{label} must take no lore counter when no Saga was declared"
        );
    }
}

/// A vanilla synthetic board: `creatures` P0 creatures c1.., `artifacts` P0
/// noncreature artifacts r1.., used by the D-16 slot-path rows.
struct SlotPathBoard {
    runner: GameRunner,
    creatures: Vec<ObjectId>,
    artifacts: Vec<ObjectId>,
}

fn slot_path_board(creatures: usize, artifacts: usize) -> SlotPathBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature_ids: Vec<ObjectId> = (1..=creatures)
        .map(|i| scenario.add_creature(P0, &format!("c{i}"), 2, 2).id())
        .collect();
    let artifact_ids: Vec<ObjectId> = (1..=artifacts)
        .map(|i| {
            scenario
                .add_artifact_from_oracle(P0, &format!("r{i}"), "")
                .id()
        })
        .collect();
    SlotPathBoard {
        runner: scenario.build(),
        creatures: creature_ids,
        artifacts: artifact_ids,
    }
}

fn counter_node(filter: TargetFilter, spec: Option<MultiTargetSpec>) -> ResolvedAbility {
    let mut node = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: filter,
        },
        vec![],
        ObjectId(999_999),
        P0,
    );
    node.multi_target = spec;
    node
}

/// Chain `nodes` head-first, each one the `sub_ability` of its predecessor.
fn chain_nodes(mut nodes: Vec<ResolvedAbility>) -> ResolvedAbility {
    let mut acc = nodes.pop().expect("a chain needs at least one node");
    while let Some(mut node) = nodes.pop() {
        node.sub_ability = Some(Box::new(acc));
        acc = node;
    }
    acc
}

fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn artifact_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
}

/// The declared targets of every node of a resolution chain, head first.
fn chain_targets(root: &ResolvedAbility) -> Vec<Vec<TargetRef>> {
    let mut out = Vec::new();
    let mut node = Some(root);
    while let Some(current) = node {
        out.push(current.targets.clone());
        node = current.sub_ability.as_deref();
    }
    out
}

/// Build the prompt for `ability`, declare `wants` slot by slot (each slot takes
/// the next wanted target when it is legal there, else it is declined), and
/// assign the declaration through the slot path.
fn assign_declared_slots(
    board: &SlotPathBoard,
    ability: &mut ResolvedAbility,
    wants: &[TargetRef],
) -> Result<(), String> {
    let state = board.runner.state();
    let slots = build_target_slots(state, ability).map_err(|err| format!("{err:?}"))?;
    let mut next = 0;
    let selection: Vec<Option<TargetRef>> = slots
        .iter()
        .map(|slot| match wants.get(next) {
            Some(want) if slot.legal_targets.contains(want) => {
                next += 1;
                Some(want.clone())
            }
            _ => None,
        })
        .collect();
    assign_selected_slots_in_chain(state, ability, &selection).map_err(|err| format!("{err:?}"))
}

/// D-16 (i). CR 601.2c + CR 115.1: an unlimited node's own slot group is the
/// declared window minus the slots the rest of the chain collects, so a later
/// min-0 node keeps its own declared targets.
///
/// RED AT BASE: the head node swallows the whole declaration
/// (`[c1, c2, r1, r2]` / `[]`).
#[test]
fn slot_path_unlimited_node_keeps_its_own_group_before_min_zero_follower() {
    let board = slot_path_board(3, 3);
    let mut chain = chain_nodes(vec![
        counter_node(creature_filter(), Some(MultiTargetSpec::unlimited(0))),
        counter_node(artifact_filter(), Some(MultiTargetSpec::unlimited(0))),
    ]);
    let wants: Vec<TargetRef> = [
        board.creatures[0],
        board.creatures[1],
        board.artifacts[0],
        board.artifacts[1],
    ]
    .into_iter()
    .map(TargetRef::Object)
    .collect();

    assign_declared_slots(&board, &mut chain, &wants).expect("the declaration must assign");

    assert_eq!(
        chain_targets(&chain),
        vec![
            vec![
                TargetRef::Object(board.creatures[0]),
                TargetRef::Object(board.creatures[1])
            ],
            vec![
                TargetRef::Object(board.artifacts[0]),
                TargetRef::Object(board.artifacts[1])
            ],
        ],
        "each node keeps its own declared group, in declared order"
    );
}

/// D-16 (ii). CR 601.2c: a bounded node declared below its bound does not take
/// the later node's declared slots.
///
/// RED AT BASE: the head node takes `[c1, r1]` and the follower `[]`.
#[test]
fn slot_path_up_to_node_below_its_bound_keeps_its_own_group() {
    let board = slot_path_board(2, 3);
    let mut chain = chain_nodes(vec![
        counter_node(creature_filter(), Some(MultiTargetSpec::fixed(0, 3))),
        counter_node(artifact_filter(), Some(MultiTargetSpec::fixed(0, 3))),
    ]);
    let wants: Vec<TargetRef> = [board.creatures[0], board.artifacts[0]]
        .into_iter()
        .map(TargetRef::Object)
        .collect();

    assign_declared_slots(&board, &mut chain, &wants).expect("the declaration must assign");

    assert_eq!(
        chain_targets(&chain),
        vec![
            vec![TargetRef::Object(board.creatures[0])],
            vec![TargetRef::Object(board.artifacts[0])],
        ],
        "the bounded node keeps only its own declared target"
    );
}

/// D-16 (iii). CR 107.1c + CR 115.6: both groups may be declared empty.
///
/// GREEN AT BASE — labelled. Paired with its red-at-base sibling
/// `slot_path_unlimited_node_keeps_its_own_group_before_min_zero_follower`;
/// pairing MP-WINREV.
#[test]
fn slot_path_both_groups_declared_empty() {
    let board = slot_path_board(3, 3);
    let mut chain = chain_nodes(vec![
        counter_node(creature_filter(), Some(MultiTargetSpec::unlimited(0))),
        counter_node(artifact_filter(), Some(MultiTargetSpec::unlimited(0))),
    ]);

    assign_declared_slots(&board, &mut chain, &[]).expect("an empty declaration must assign");

    assert_eq!(
        chain_targets(&chain),
        vec![Vec::new(), Vec::new()],
        "neither node receives a target"
    );
}

/// D-16 (iv). CR 115.1: an unlimited node before a required single slot leaves
/// that slot its declared target.
///
/// GREEN AT BASE — labelled. Paired with
/// `slot_path_unlimited_node_keeps_its_own_group_before_min_zero_follower`;
/// pairing MP-WINREV.
#[test]
fn slot_path_unlimited_node_before_required_slot_preserved() {
    let board = slot_path_board(3, 3);
    let mut chain = chain_nodes(vec![
        counter_node(creature_filter(), Some(MultiTargetSpec::unlimited(0))),
        counter_node(artifact_filter(), None),
    ]);
    let wants: Vec<TargetRef> = [board.creatures[0], board.creatures[1], board.artifacts[0]]
        .into_iter()
        .map(TargetRef::Object)
        .collect();

    assign_declared_slots(&board, &mut chain, &wants).expect("the declaration must assign");

    assert_eq!(
        chain_targets(&chain),
        vec![
            vec![
                TargetRef::Object(board.creatures[0]),
                TargetRef::Object(board.creatures[1])
            ],
            vec![TargetRef::Object(board.artifacts[0])],
        ],
        "the required slot keeps its own declared target"
    );
}

/// D-16 (v). CR 115.6: a bounded group declared partly empty does not borrow
/// the later group's declared slots.
///
/// GREEN AT BASE — labelled. Paired with
/// `slot_path_up_to_node_below_its_bound_keeps_its_own_group`; pairing
/// MP-WINREV.
#[test]
fn slot_path_up_to_node_partly_declined_preserved() {
    let board = slot_path_board(3, 3);
    let mut chain = chain_nodes(vec![
        counter_node(creature_filter(), Some(MultiTargetSpec::fixed(0, 2))),
        counter_node(artifact_filter(), Some(MultiTargetSpec::fixed(0, 3))),
    ]);
    let wants: Vec<TargetRef> = [board.creatures[0], board.artifacts[0], board.artifacts[1]]
        .into_iter()
        .map(TargetRef::Object)
        .collect();

    assign_declared_slots(&board, &mut chain, &wants).expect("the declaration must assign");

    assert_eq!(
        chain_targets(&chain),
        vec![
            vec![TargetRef::Object(board.creatures[0])],
            vec![
                TargetRef::Object(board.artifacts[0]),
                TargetRef::Object(board.artifacts[1])
            ],
        ],
        "the partly declined group keeps exactly what was declared for it"
    );
}

/// D-16 (vi-b). CR 601.2c: a later node whose filter is controller-relative
/// mints its own companion player slot, and that slot plus the node's declared
/// object stay with the node.
///
/// RED AT BASE: the assignment fails with a missing-required-target error.
#[test]
fn slot_path_prior_binding_companion_player_slot_stays_with_its_node() {
    let board = slot_path_board(3, 3);
    let player_node = ResolvedAbility::new(
        Effect::TargetOnly {
            target: TargetFilter::Player,
        },
        vec![],
        ObjectId(999_999),
        P0,
    );
    let mut chain = chain_nodes(vec![
        player_node,
        counter_node(creature_filter(), Some(MultiTargetSpec::unlimited(0))),
        counter_node(
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::TargetPlayer),
            ),
            Some(MultiTargetSpec::unlimited(0)),
        ),
    ]);
    let wants = vec![
        TargetRef::Player(P0),
        TargetRef::Object(board.creatures[0]),
        TargetRef::Player(P0),
        TargetRef::Object(board.artifacts[0]),
    ];

    assign_declared_slots(&board, &mut chain, &wants).expect("the declaration must assign");

    assert_eq!(
        chain_targets(&chain),
        vec![
            vec![TargetRef::Player(P0)],
            vec![TargetRef::Object(board.creatures[0])],
            vec![TargetRef::Player(P0), TargetRef::Object(board.artifacts[0])],
        ],
        "the companion player slot stays with the node whose filter reads it"
    );
}

/// The declared targets of each node of the top stack entry's resolution chain,
/// head first, paired with that node's effect kind.
fn top_stack_chain(runner: &GameRunner) -> Vec<(EffectKind, Vec<TargetRef>)> {
    let Some(entry) = runner.state().stack.last() else {
        return Vec::new();
    };
    let Some(root) = entry.ability() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut node = Some(root);
    while let Some(current) = node {
        out.push((EffectKind::from(&current.effect), current.targets.clone()));
        node = current.sub_ability.as_deref();
    }
    out
}

/// D-17. CR 115.6 + CR 601.2c: Explosive Entry announces two independent "up to
/// one target" sets. With no artifact on the battlefield the first set is
/// necessarily empty, and the declared creature belongs to the second — the
/// counter instruction — not to the Destroy.
///
/// RED AT BASE: the Destroy node takes the declared creature and the counter
/// node is left empty, so no counter is placed.
#[test]
fn explosive_entry_with_no_artifact_counters_the_declared_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "c1", 2, 2).id();
    let c2 = scenario.add_creature(P0, "c2", 2, 2).id();
    let entry = scenario
        .add_spell_to_hand_from_oracle(P0, "Explosive Entry", false, EXPLOSIVE_ENTRY_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Red));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let card_id = runner.state().objects[&entry].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: entry,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("Explosive Entry must be castable");

    to_target_prompt(&mut runner, "Explosive Entry targets");
    let outcomes = declare_slot_by_slot(&mut runner, &[TargetRef::Object(c1)]);
    assert_eq!(
        first_declaration_error(&outcomes),
        None,
        "the declaration must be accepted"
    );

    let chain = top_stack_chain(&runner);
    let destroy = chain
        .iter()
        .find(|(kind, _)| *kind == EffectKind::Destroy)
        .expect("reach guard: the chain must hold the Destroy node");
    let counter = chain
        .iter()
        .find(|(kind, _)| *kind == EffectKind::PutCounter)
        .expect("reach guard: the chain must hold the counter node");
    assert!(
        destroy.1.is_empty(),
        "CR 115.6: with no artifact in play the Destroy set is empty, got {:?}",
        destroy.1
    );
    assert_eq!(
        counter.1,
        vec![TargetRef::Object(c1)],
        "the declared creature belongs to the counter instruction"
    );

    let events = drain_stack_collecting_events(&mut runner);
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Destroy,
                ..
            }
        )),
        "the Destroy instruction still resolves"
    );
    assert_eq!(put_counter_resolutions(&events), 1);
    assert_eq!(
        counters(&runner, c1, CounterType::Plus1Plus1),
        1,
        "the declared creature takes its +1/+1 counter"
    );
    assert_eq!(
        counters(&runner, c2, CounterType::Plus1Plus1),
        0,
        "CR 115.10a: the undeclared creature takes nothing"
    );
}

// ── D-18 — declared-target anaphor binding (U4) ─────────────────────────────
//
// Verbatim Oracle text (MTGJSON `AtomicCards.json`):
//
// - Stensia Innkeeper: "When this creature enters, tap target land an opponent
//   controls. That land doesn't untap during its controller's next untap step."
// - Kenku Artificer: "Homunculus Servant — When this creature enters, put three
//   +1/+1 counters on up to one target noncreature artifact. That artifact
//   becomes a 0/0 Homunculus artifact creature with flying."
// - Guardian of Tazeem: "Flying\nLandfall — Whenever a land you control enters,
//   tap target creature an opponent controls. If that land is an Island, that
//   creature doesn't untap during its controller's next untap step."
// - Magitek Scythe: "A Test of Your Reflexes! — When this Equipment enters, you
//   may attach it to target creature you control. If you do, that creature gains
//   first strike until end of turn and must be blocked this turn if able.\n
//   Equipped creature gets +2/+1.\nEquip {2}"
// - Neyith of the Dire Hunt: "Whenever one or more creatures you control fight
//   or become blocked, draw a card.\nAt the beginning of combat on your turn,
//   you may pay {2}{R/G}. If you do, double target creature's power until end of
//   turn. That creature must be blocked this combat if able."
// - Spiked Ripsaw: "Equipped creature gets +3/+3.\nWhenever equipped creature
//   attacks, you may sacrifice a Forest. If you do, that creature gains trample
//   until end of turn.\nEquip {3}"

const STENSIA_INNKEEPER_ORACLE: &str = "When this creature enters, tap target land an opponent controls. That land doesn't untap during its controller's next untap step.";

const KENKU_ARTIFICER_ORACLE: &str = "Homunculus Servant — When this creature enters, put three +1/+1 counters on up to one target noncreature artifact. That artifact becomes a 0/0 Homunculus artifact creature with flying.";

const GUARDIAN_OF_TAZEEM_ORACLE: &str = "Flying\nLandfall — Whenever a land you control enters, tap target creature an opponent controls. If that land is an Island, that creature doesn't untap during its controller's next untap step.";

const MAGITEK_SCYTHE_ORACLE: &str = "A Test of Your Reflexes! — When this Equipment enters, you may attach it to target creature you control. If you do, that creature gains first strike until end of turn and must be blocked this turn if able.\nEquipped creature gets +2/+1.\nEquip {2}";

const NEYITH_ORACLE: &str = "Whenever one or more creatures you control fight or become blocked, draw a card.\nAt the beginning of combat on your turn, you may pay {2}{R/G}. If you do, double target creature's power until end of turn. That creature must be blocked this combat if able.";

const SPIKED_RIPSAW_ORACLE: &str = "Equipped creature gets +3/+3.\nWhenever equipped creature attacks, you may sacrifice a Forest. If you do, that creature gains trample until end of turn.\nEquip {3}";

/// Objects named directly by a transient continuous effect carrying a
/// modification `pred` accepts, in effect order.
fn tce_recipients_where(
    runner: &GameRunner,
    pred: impl Fn(&ContinuousModification) -> bool,
) -> Vec<ObjectId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|tce| tce.modifications.iter().any(&pred))
        .filter_map(|tce| match tce.affected {
            TargetFilter::SpecificObject { id } => Some(id),
            _ => None,
        })
        .collect()
}

fn grants_keyword(modification: &ContinuousModification, wanted: Keyword) -> bool {
    matches!(modification, ContinuousModification::AddKeyword { keyword } if *keyword == wanted)
}

fn grants_cant_untap(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddStaticMode {
            mode: StaticMode::CantUntap
        }
    )
}

fn grants_must_be_blocked(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddStaticMode {
            mode: StaticMode::MustBeBlocked { .. }
        }
    )
}

fn adds_power(modification: &ContinuousModification) -> bool {
    matches!(modification, ContinuousModification::AddPower { .. })
}

fn live(runner: &GameRunner, id: ObjectId) -> &engine::game::game_object::GameObject {
    &runner.state().objects[&id]
}

/// Drive a real board until the stack is empty: order triggers, answer every
/// optional-effect choice with `accept` (adding `mana` green mana to the
/// choosing player first, for a payment gate), declare `wants` slot by slot, and
/// pass priority otherwise.
fn drive_board(
    runner: &mut GameRunner,
    wants: &[TargetRef],
    accept: bool,
    mana: usize,
    what: &str,
) {
    let mut next = 0;
    let mut paid = false;
    let mut observed = Vec::new();
    for _ in 0..60 {
        let waiting = runner.state().waiting_for.clone();
        observed.push(waiting_label(&waiting));
        match waiting {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let legal = current_slot_legal_targets(runner);
                let pick = wants.get(next).filter(|want| legal.contains(want)).cloned();
                if pick.is_some() {
                    next += 1;
                }
                runner
                    .act(GameAction::ChooseTarget { target: pick })
                    .unwrap_or_else(|err| panic!("{what}: slot declaration rejected: {err:?}"));
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                if accept && !paid && mana > 0 {
                    for _ in 0..mana {
                        let _ = runner.state_mut().add_mana_to_pool(
                            player,
                            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
                        );
                    }
                    paid = true;
                }
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .unwrap_or_else(|err| panic!("{what}: optional choice rejected: {err:?}"));
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance resolution");
            }
            other => panic!("{what}: unexpected {other:?}; observed {observed:#?}"),
        }
    }
    panic!("{what}: the stack did not empty in 60 steps; observed {observed:#?}");
}

/// D-18 (Stensia Innkeeper). CR 608.2c: "That land" names the land the previous
/// instruction declared as its target, so the don't-untap restriction lands on
/// that land and not on the trigger's source.
///
/// RED AT BASE: the restriction registers on Stensia Innkeeper.
#[test]
fn stensia_innkeeper_that_land_doesnt_untap_is_the_targeted_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opp_land = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Red);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    let stensia = scenario
        .add_creature_to_hand_from_oracle(P0, "Stensia Innkeeper", 3, 3, STENSIA_INNKEEPER_ORACLE)
        .with_subtypes(vec!["Vampire"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Red));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(stensia).commit();

    drive_board(
        &mut runner,
        &[TargetRef::Object(opp_land)],
        true,
        0,
        "Stensia Innkeeper ETB",
    );

    assert!(
        live(&runner, opp_land).tapped,
        "reach guard: the declared land was tapped, so the trigger resolved"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_cant_untap),
        vec![opp_land],
        "CR 608.2c: the don't-untap restriction belongs to the declared land"
    );
    assert!(
        !tce_recipients(&runner).contains(&stensia),
        "no continuous effect may name the trigger's source"
    );
}

struct KenkuBoard {
    runner: GameRunner,
    kenku: ObjectId,
    relic: ObjectId,
    relic2: ObjectId,
}

fn kenku_board() -> KenkuBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let relic = scenario.add_artifact_from_oracle(P0, "Relic", "").id();
    let relic2 = scenario.add_artifact_from_oracle(P0, "Relic Two", "").id();
    let kenku = scenario
        .add_creature_to_hand_from_oracle(P0, "Kenku Artificer", 1, 1, KENKU_ARTIFICER_ORACLE)
        .with_subtypes(vec!["Bird", "Artificer"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Blue));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(kenku).commit();
    KenkuBoard {
        runner,
        kenku,
        relic,
        relic2,
    }
}

/// D-18 (Kenku Artificer, declared). CR 608.2c: "That artifact" names the
/// declared target of the counter instruction, so the declared artifact — not
/// Kenku Artificer itself — becomes a 0/0 Homunculus artifact creature with
/// flying, and survives at 3/3 on its three +1/+1 counters.
///
/// RED AT BASE: the type change lands on Kenku Artificer, which is then a 0/0
/// with no counters and dies.
#[test]
fn kenku_artificer_declared_artifact_becomes_the_creature() {
    let mut board = kenku_board();
    let relic = board.relic;
    drive_board(
        &mut board.runner,
        &[TargetRef::Object(relic)],
        true,
        0,
        "Kenku Artificer ETB",
    );

    let declared = live(&board.runner, relic);
    assert!(
        declared.card_types.core_types.contains(&CoreType::Artifact)
            && declared.card_types.core_types.contains(&CoreType::Creature),
        "CR 608.2c: the declared artifact becomes an artifact creature, got {:?}",
        declared.card_types.core_types
    );
    assert!(
        declared
            .card_types
            .subtypes
            .contains(&"Homunculus".to_string()),
        "the declared artifact gains the Homunculus subtype, got {:?}",
        declared.card_types.subtypes
    );
    assert!(
        has_keyword(declared, &Keyword::Flying),
        "the declared artifact gains flying"
    );
    assert_eq!(
        counters(&board.runner, relic, CounterType::Plus1Plus1),
        3,
        "the declared artifact carries the three +1/+1 counters"
    );
    assert_eq!(
        (declared.power, declared.toughness),
        (Some(3), Some(3)),
        "base 0/0 plus three +1/+1 counters is 3/3"
    );

    let untouched = live(&board.runner, board.relic2);
    assert!(
        !untouched
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "CR 115.10a: the undeclared artifact is unchanged"
    );
    assert_eq!(
        live(&board.runner, board.kenku).zone,
        Zone::Battlefield,
        "Kenku Artificer keeps its own characteristics and stays on the battlefield"
    );
}

/// D-18 (Kenku Artificer, declined). CR 115.6 + CR 608.2c: with the optional
/// target declined, "That artifact" names nothing, so no object becomes a 0/0
/// creature — least of all the source.
///
/// RED AT BASE: the type change lands on Kenku Artificer, which dies as a 0/0.
#[test]
fn kenku_artificer_declined_grants_nothing_and_kenku_survives() {
    let mut board = kenku_board();
    drive_board(&mut board.runner, &[], true, 0, "Kenku Artificer ETB");

    assert_eq!(
        tce_affected(&board.runner),
        Vec::new(),
        "a declined optional target installs no continuous effect"
    );
    assert_eq!(
        live(&board.runner, board.kenku).zone,
        Zone::Battlefield,
        "Kenku Artificer stays on the battlefield"
    );
}

/// D-18 (Guardian of Tazeem). CR 608.2c: the landfall trigger declares a
/// creature, and "that creature" in the Island rider names that same declared
/// creature — the land it also names is a separate anaphor.
///
/// RED AT BASE: the restriction registers on the Island that triggered it.
#[test]
fn guardian_of_tazeem_that_creature_doesnt_untap_is_the_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Guardian of Tazeem", 4, 5, GUARDIAN_OF_TAZEEM_ORACLE)
        .with_subtypes(vec!["Sphinx"]);
    let opp = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let island = scenario
        .add_land_to_hand(P0, "Island")
        .with_subtypes(vec!["Island"])
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let card_id = live(&runner, island).card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: island,
            card_id,
        })
        .expect("the Island must be playable");

    drive_board(
        &mut runner,
        &[TargetRef::Object(opp)],
        true,
        0,
        "Guardian of Tazeem landfall",
    );

    assert!(
        live(&runner, opp).tapped,
        "reach guard: the declared creature was tapped, so the trigger resolved"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_cant_untap),
        vec![opp],
        "CR 608.2c: the don't-untap restriction belongs to the declared creature"
    );
    assert!(
        !tce_recipients(&runner).contains(&island),
        "the Island that triggered landfall gains nothing"
    );
}

struct ScytheBoard {
    runner: GameRunner,
    scythe: ObjectId,
    c2: ObjectId,
}

fn scythe_board() -> ScytheBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "c1", 2, 2);
    let c2 = scenario.add_creature(P0, "c2", 2, 2).id();
    let scythe = scenario
        .add_artifact_to_hand_from_oracle(P0, "Magitek Scythe", MAGITEK_SCYTHE_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(scythe).commit();
    ScytheBoard { runner, scythe, c2 }
}

/// D-18 (Magitek Scythe, accepted). CR 118.12 + CR 608.2c: an affirmative "if
/// you do" gate decides whether its instruction happens, not what that
/// instruction declared — so both grants in the gated sentence name the
/// declared creature.
///
/// RED AT BASE: the must-be-blocked grant registers on the Equipment.
#[test]
fn magitek_scythe_if_you_do_that_creature_must_be_blocked_is_the_target() {
    let mut board = scythe_board();
    let c2 = board.c2;
    drive_board(
        &mut board.runner,
        &[TargetRef::Object(c2)],
        true,
        0,
        "Magitek Scythe ETB",
    );

    assert_eq!(
        tce_recipients_where(&board.runner, |m| grants_keyword(m, Keyword::FirstStrike)),
        vec![c2],
        "the first-strike grant belongs to the declared creature"
    );
    assert_eq!(
        tce_recipients_where(&board.runner, grants_must_be_blocked),
        vec![c2],
        "CR 509.1c + CR 608.2c: so does the must-be-blocked requirement"
    );
    assert!(
        !tce_recipients(&board.runner).contains(&board.scythe),
        "the Equipment itself gains neither"
    );
    assert!(
        has_keyword(live(&board.runner, c2), &Keyword::FirstStrike),
        "the declared creature actually has first strike"
    );
}

/// D-18 (Magitek Scythe, declined). CR 118.12: the whole gated sentence is
/// conditional on the attach happening, so declining grants nothing.
///
/// GREEN AT BASE — labelled. It is reach-guarded by
/// `magitek_scythe_if_you_do_that_creature_must_be_blocked_is_the_target`,
/// which accepts on the identical board and does install grants. No in-scope
/// mutation turns this row red: it is a preservation row.
#[test]
fn magitek_scythe_declined_attach_grants_nothing() {
    let mut board = scythe_board();
    let c2 = board.c2;
    drive_board(
        &mut board.runner,
        &[TargetRef::Object(c2)],
        false,
        0,
        "Magitek Scythe ETB",
    );

    assert_eq!(
        tce_affected(&board.runner),
        Vec::new(),
        "CR 118.12: a declined gate installs no continuous effect"
    );
    assert_eq!(
        live(&board.runner, c2).power,
        Some(2),
        "the creature is unchanged"
    );
}

/// D-18 (Neyith of the Dire Hunt, paid). CR 118.12 + CR 608.2c: the payment
/// gates the doubling, and the separate sentence's "That creature" still names
/// the declared target, which exists whether or not the cost was paid
/// (CR 603.3d).
///
/// RED AT BASE: only the doubling registers; the must-be-blocked requirement is
/// lost to the trigger's source.
#[test]
fn neyith_if_you_do_that_creature_must_be_blocked_is_the_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Neyith of the Dire Hunt", 3, 3, NEYITH_ORACLE);
    scenario.add_creature(P0, "c1", 2, 2);
    let c2 = scenario.add_creature(P0, "c2", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);

    drive_board(
        &mut runner,
        &[TargetRef::Object(c2)],
        true,
        3,
        "Neyith begin-combat trigger",
    );

    assert_eq!(
        tce_recipients_where(&runner, adds_power),
        vec![c2],
        "the doubling applies to the declared creature"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_must_be_blocked),
        vec![c2],
        "CR 608.2c + CR 509.1c: so does the must-be-blocked requirement"
    );
    assert_eq!(
        live(&runner, c2).power,
        Some(4),
        "reach guard: the payment happened, so the declared creature's power doubled"
    );
}

/// D-18 (Spiked Ripsaw) — PRESERVATION. CR 608.2c: "that creature" here names
/// the attacking creature the trigger is about, which is not a declared target,
/// so the trample grant must keep naming the attacker.
///
/// GREEN AT BASE — labelled; its pairing is MP-U4-OVERREACH.
#[test]
fn spiked_ripsaw_trample_stays_on_the_attacker() {
    for accept in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::DeclareAttackers);
        let bearer = scenario.add_creature(P0, "Bearer", 2, 2).id();
        let other = scenario.add_creature(P0, "Other Attacker", 2, 2).id();
        let ripsaw = scenario
            .add_artifact_from_oracle(P0, "Spiked Ripsaw", SPIKED_RIPSAW_ORACLE)
            .with_subtypes(vec!["Equipment"])
            .id();
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.objects.get_mut(&ripsaw).unwrap().attached_to =
                Some(engine::game::game_object::AttachTarget::Object(bearer));
            state
                .objects
                .get_mut(&bearer)
                .unwrap()
                .attachments
                .push(ripsaw);
        }
        runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![
                    (bearer, AttackTarget::Player(P1)),
                    (other, AttackTarget::Player(P1)),
                ],
                bands: vec![],
            })
            .expect("both creatures attack P1");

        drive_board(&mut runner, &[], accept, 0, "Spiked Ripsaw attack trigger");

        let expected: Vec<ObjectId> = if accept { vec![bearer] } else { Vec::new() };
        assert_eq!(
            tce_recipients_where(&runner, |m| grants_keyword(m, Keyword::Trample)),
            expected,
            "accept={accept}: trample must stay on the equipped attacker (CR 608.2c)"
        );
        assert_eq!(
            has_keyword(live(&runner, bearer), &Keyword::Trample),
            accept,
            "accept={accept}: the attacker's live keywords must agree"
        );
        assert!(
            !has_keyword(live(&runner, other), &Keyword::Trample),
            "accept={accept}: the other attacker never gains trample"
        );
    }
}

// ── S-RW — a bare "it" after a declared target (U4) ─────────────────────────
//
// Verbatim Oracle text (MTGJSON `AtomicCards.json`):
//
// - Rootwise Survivor: "Haste\nSurvival — At the beginning of your second main
//   phase, if this creature is tapped, put three +1/+1 counters on up to one
//   target land you control. That land becomes a 0/0 Elemental creature in
//   addition to its other types. It gains haste until your next turn."
// - Academic Dispute: "Target creature blocks this turn if able. You may have it
//   gain reach until end of turn.\nLearn."
// - Legion Leadership: "Until end of turn, double target creature's power and it
//   gains first strike."
// - Dominus of Fealty: "Flying\nAt the beginning of your upkeep, you may gain
//   control of target permanent until end of turn. If you do, untap it and it
//   gains haste until end of turn."
// - Samite Alchemist: "{W}{W}, {T}: Prevent the next 4 damage that would be
//   dealt this turn to target creature you control. Tap that creature. It
//   doesn't untap during your next untap step."

const ROOTWISE_SURVIVOR_ORACLE: &str = "Haste\nSurvival — At the beginning of your second main phase, if this creature is tapped, put three +1/+1 counters on up to one target land you control. That land becomes a 0/0 Elemental creature in addition to its other types. It gains haste until your next turn.";

const ACADEMIC_DISPUTE_ORACLE: &str =
    "Target creature blocks this turn if able. You may have it gain reach until end of turn.\nLearn.";

const LEGION_LEADERSHIP_ORACLE: &str =
    "Until end of turn, double target creature's power and it gains first strike.";

const DOMINUS_OF_FEALTY_ORACLE: &str = "Flying\nAt the beginning of your upkeep, you may gain control of target permanent until end of turn. If you do, untap it and it gains haste until end of turn.";

const SAMITE_ALCHEMIST_ORACLE: &str = "{W}{W}, {T}: Prevent the next 4 damage that would be dealt this turn to target creature you control. Tap that creature. It doesn't untap during your next untap step.";

/// P0's c1, c2 and Forest l0; P1's o1, o2 and Forest l1.
struct AnaphorFixtures {
    c1: ObjectId,
    o1: ObjectId,
    l0: ObjectId,
}

fn anaphor_fixtures(scenario: &mut GameScenario) -> AnaphorFixtures {
    let c1 = scenario.add_creature(P0, "c1", 2, 2).id();
    scenario.add_creature(P0, "c2", 2, 2);
    let o1 = scenario.add_creature(P1, "o1", 2, 2).id();
    scenario.add_creature(P1, "o2", 2, 2);
    let l0 = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    AnaphorFixtures { c1, o1, l0 }
}

/// Cast `name` from P0's hand on the shared anaphor board, declaring `declare`,
/// and drive the resolution to the end of the stack.
fn cast_anaphor_spell(
    name: &str,
    text: &str,
    instant: bool,
    declare: impl Fn(&AnaphorFixtures) -> Vec<ObjectId>,
) -> (GameRunner, AnaphorFixtures, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fixtures = anaphor_fixtures(&mut scenario);
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, text)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let declared = declare(&fixtures);
    runner.cast(card).target_objects(&declared).commit();
    drive_board(&mut runner, &[], true, 0, name);
    (runner, fixtures, card)
}

struct RootwiseBoard {
    runner: GameRunner,
    src: ObjectId,
    l0: ObjectId,
}

/// Rootwise Survivor tapped on P0's battlefield, advanced to the second main
/// phase so its Survival trigger goes on the stack.
fn rootwise_board() -> RootwiseBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::EndCombat);
    let fixtures = anaphor_fixtures(&mut scenario);
    let src = scenario
        .add_creature_from_oracle(P0, "Rootwise Survivor", 3, 4, ROOTWISE_SURVIVOR_ORACLE)
        .with_subtypes(vec!["Human", "Survivor"])
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&src).unwrap().tapped = true;
    runner.advance_to_phase(Phase::PostCombatMain);
    RootwiseBoard {
        runner,
        src,
        l0: fixtures.l0,
    }
}

/// S-RW (Rootwise Survivor, declared). CR 608.2c + CR 601.2c: the chain's
/// nearest antecedent for the bare "It" is the land the first instruction
/// declared — which the middle sentence already bound — so the haste grant
/// names that land, not the source.
///
/// RED AT BASE: the haste grant registers on Rootwise Survivor, and the
/// animated land has no keyword at all.
#[test]
fn rootwise_survivor_it_gains_haste_is_the_declared_land() {
    let mut board = rootwise_board();
    let l0 = board.l0;
    drive_board(
        &mut board.runner,
        &[TargetRef::Object(l0)],
        true,
        0,
        "Rootwise Survivor Survival trigger",
    );

    let land = live(&board.runner, l0);
    assert!(
        land.card_types.core_types.contains(&CoreType::Land)
            && land.card_types.core_types.contains(&CoreType::Creature),
        "the declared land becomes a creature in addition to its other types, got {:?}",
        land.card_types.core_types
    );
    assert!(
        land.card_types.subtypes.contains(&"Elemental".to_string()),
        "the declared land gains the Elemental subtype, got {:?}",
        land.card_types.subtypes
    );
    assert_eq!(
        counters(&board.runner, l0, CounterType::Plus1Plus1),
        3,
        "reach guard: the counters were placed on the declared land"
    );
    assert_eq!(
        (land.power, land.toughness),
        (Some(3), Some(3)),
        "base 0/0 plus three +1/+1 counters is 3/3"
    );
    assert_eq!(
        tce_recipients_where(&board.runner, |m| grants_keyword(m, Keyword::Haste)),
        vec![l0],
        "CR 608.2c: \"It gains haste\" names the declared land"
    );
    assert!(
        !tce_recipients_where(&board.runner, |m| grants_keyword(m, Keyword::Haste))
            .contains(&board.src),
        "the source keeps nothing from the anaphor"
    );
}

/// S-RW (Rootwise Survivor, declined). CR 115.6 + CR 608.2c: with the optional
/// land declined, the bare "It" names nothing and no object gains haste.
///
/// RED AT BASE: the haste grant registers on Rootwise Survivor.
///
/// Reach-guarded by `rootwise_survivor_it_gains_haste_is_the_declared_land`,
/// which declares the land on the identical board and does install the grant.
#[test]
fn rootwise_survivor_declined_land_grants_no_haste() {
    let mut board = rootwise_board();
    drive_board(
        &mut board.runner,
        &[],
        true,
        0,
        "Rootwise Survivor Survival trigger",
    );

    assert_eq!(
        tce_recipients_where(&board.runner, |m| grants_keyword(m, Keyword::Haste)),
        Vec::new(),
        "CR 115.6: a declined target grants haste to nobody"
    );
}

/// S-RW (Academic Dispute). CR 608.2c: "You may have it gain reach" names the
/// spell's declared target.
///
/// RED AT BASE: the grant registers on the spell card itself.
#[test]
fn academic_dispute_you_may_have_it_gain_reach_is_the_target() {
    let (runner, fixtures, card) = cast_anaphor_spell(
        "Academic Dispute",
        ACADEMIC_DISPUTE_ORACLE,
        true,
        |fixtures| vec![fixtures.o1],
    );

    assert_eq!(
        tce_recipients_where(&runner, |m| grants_keyword(m, Keyword::Reach)),
        vec![fixtures.o1],
        "CR 608.2c: reach lands on the declared creature"
    );
    assert!(
        !tce_recipients(&runner).contains(&card),
        "the spell card gains nothing"
    );
}

/// S-RW (Legion Leadership). CR 608.2c: "and it gains first strike" names the
/// same declared creature the doubling applies to.
///
/// RED AT BASE: the grant registers on the spell card itself.
#[test]
fn legion_leadership_it_gains_first_strike_is_the_target() {
    let (runner, fixtures, card) = cast_anaphor_spell(
        "Legion Leadership",
        LEGION_LEADERSHIP_ORACLE,
        true,
        |fixtures| vec![fixtures.c1],
    );

    assert_eq!(
        tce_recipients_where(&runner, |m| grants_keyword(m, Keyword::FirstStrike)),
        vec![fixtures.c1],
        "CR 608.2c: first strike lands on the declared creature"
    );
    assert!(
        !tce_recipients(&runner).contains(&card),
        "the spell card gains nothing"
    );
    assert_eq!(
        live(&runner, fixtures.c1).power,
        Some(4),
        "reach guard: the declared creature's power was doubled"
    );
}

/// S-RW (Dominus of Fealty). CR 118.12 + CR 608.2c: an affirmative "If you do"
/// gate does not break the anaphor chain, so "untap it and it gains haste"
/// names the permanent the trigger declared.
///
/// RED AT BASE: the haste grant registers on Dominus of Fealty.
#[test]
fn dominus_of_fealty_if_you_do_it_gains_haste_is_the_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let fixtures = anaphor_fixtures(&mut scenario);
    let src = scenario
        .add_creature_from_oracle(P0, "Dominus of Fealty", 4, 4, DOMINUS_OF_FEALTY_ORACLE)
        .with_subtypes(vec!["Spirit", "Avatar"])
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&fixtures.o1)
        .unwrap()
        .tapped = true;
    runner.advance_to_phase(Phase::Upkeep);

    drive_board(
        &mut runner,
        &[TargetRef::Object(fixtures.o1)],
        true,
        0,
        "Dominus of Fealty upkeep trigger",
    );

    assert_eq!(
        live(&runner, fixtures.o1).controller,
        P0,
        "reach guard: control of the declared permanent changed"
    );
    assert_eq!(
        tce_recipients_where(&runner, |m| grants_keyword(m, Keyword::Haste)),
        vec![fixtures.o1],
        "CR 608.2c: haste lands on the declared permanent"
    );
    assert!(
        !tce_recipients_where(&runner, |m| grants_keyword(m, Keyword::Haste)).contains(&src),
        "the source gains nothing"
    );
}

/// S-RW (Samite Alchemist). CR 608.2c: "Tap that creature. It doesn't untap"
/// both name the ability's declared target.
///
/// RED AT BASE: the don't-untap restriction registers on Samite Alchemist.
#[test]
fn samite_alchemist_it_doesnt_untap_is_the_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fixtures = anaphor_fixtures(&mut scenario);
    let src = scenario
        .add_creature_from_oracle(P0, "Samite Alchemist", 0, 5, SAMITE_ALCHEMIST_ORACLE)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::White));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner
        .activate(src, 0)
        .target_objects(&[fixtures.c1])
        .resolve();
    runner.advance_until_stack_empty();

    assert!(
        live(&runner, fixtures.c1).tapped,
        "reach guard: the declared creature was tapped by the ability"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_cant_untap),
        vec![fixtures.c1],
        "CR 608.2c: the don't-untap restriction names the declared creature"
    );
    assert!(
        !tce_recipients_where(&runner, grants_cant_untap).contains(&src),
        "the source gains nothing"
    );
}

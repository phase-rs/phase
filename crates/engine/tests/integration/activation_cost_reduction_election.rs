//! CR 601.2f + CR 602.2b: the cost-reduction election for ACTIVATED abilities.
//!
//! CR 602.2b makes an activation cost the analog of a spell's mana cost for
//! CR 601.2f: "The total cost is the mana cost or alternative cost ..., plus all
//! additional costs and cost increases, and minus all cost reductions. If
//! multiple cost reductions apply, the player may apply them in any order."
//!
//! Activation reductions are generic-only (CR 118.7a), so the order matters for
//! one reason: a floor. Training Grounds "can't reduce the mana in that cost to
//! less than one mana", so on `{3}` Training Grounds (−2) then an unfloored −2
//! locks `{0}`, while the reverse locks `{1}`. Reductions with equal effective
//! floors commute.
//!
//! These tests pin the substrate the election sits on: raises are applied before
//! reductions (CR 601.2f), nothing depends on battlefield order, and the default
//! order — the one every preview uses — is the cheapest one.

use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, CostReduction, Effect,
    QuantityExpr, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::casting_costs::{
    ActivationCostLock, ActivationCostLockPoint, CostReductionEntry, ReductionProvenance,
};
use engine::types::game_state::{ActionDisposition, ActionResult, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{ActivationExemption, CostModifyMode, StaticMode};

/// Scryfall-verified Oracle text.
const TRAINING_GROUNDS: &str = "Activated abilities of creatures you control cost {2} less to activate. This effect can't reduce the mana in that cost to less than one mana.";
const SUPPRESSION_FIELD: &str =
    "Activated abilities cost {2} more to activate unless they're mana abilities.";

/// One cost modifier on the battlefield.
#[derive(Clone, Copy, Debug)]
enum Modifier {
    /// Training Grounds: −2, can't reduce below one mana.
    Grounds,
    /// Suppression Field: +2, mana abilities exempt.
    Suppression,
    /// An unfloored "activated abilities of creatures you control cost {N} less"
    /// reducer — the Professor Hojo shape, which no printed card on this tree
    /// carries yet, so it is built directly.
    Unfloored(u32),
    /// A −N reducer that can't reduce below TWO mana. No printed card carries a
    /// two-mana floor; it is the smallest floor an `{X}` cost can observe,
    /// because the `X` symbol already counts as one mana toward any floor.
    FlooredTwo(u32),
    /// A −`amount` reducer with an arbitrary floor (0 = unfloored).
    Reducer { amount: u32, floor: u32 },
}

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn reducer(amount: u32, minimum_mana: Option<u32>) -> StaticDefinition {
    StaticDefinition::new(StaticMode::ReduceAbilityCost {
        mode: CostModifyMode::Reduce,
        keyword: "activated".to_string(),
        amount,
        minimum_mana,
        dynamic_count: None,
        exemption: ActivationExemption::None,
        activator: None,
    })
    .affected(TargetFilter::Typed(
        TypedFilter::creature().controller(ControllerRef::You),
    ))
}

fn unfloored_reducer(amount: u32) -> StaticDefinition {
    reducer(amount, None)
}

/// The activated ability under test.
enum Activator {
    /// A creature with `{cost}: You gain 1 life`, plus an optional own rider.
    Built {
        cost: AbilityCost,
        rider: Option<u32>,
    },
    /// A creature whose ability is parsed from this Oracle text.
    Oracle(&'static str),
}

struct Board {
    runner: GameRunner,
    source: ObjectId,
    ability_index: usize,
}

impl Board {
    /// Modifiers are created in the order given, so reversing the slice reverses
    /// battlefield order. `rider` is the ability's own "costs {N} less" text.
    fn new(modifiers: &[Modifier], cost: AbilityCost, rider: Option<u32>, pool: usize) -> Self {
        Self::with_activator(modifiers, Activator::Built { cost, rider }, pool)
    }

    fn with_activator(modifiers: &[Modifier], activator: Activator, pool: usize) -> Self {
        Self::with_setup(modifiers, activator, pool, |_| {})
    }

    /// As [`Board::with_activator`], with `setup` run on the scenario before it
    /// is built (to add pieces that are not cost modifiers, such as lands).
    fn with_setup(
        modifiers: &[Modifier],
        activator: Activator,
        pool: usize,
        setup: impl FnOnce(&mut GameScenario),
    ) -> Self {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        setup(&mut scenario);
        for (i, modifier) in modifiers.iter().enumerate() {
            match modifier {
                Modifier::Grounds => {
                    scenario.add_enchantment_from_oracle(
                        P0,
                        &format!("Training Grounds {i}"),
                        TRAINING_GROUNDS,
                    );
                }
                Modifier::Suppression => {
                    scenario.add_enchantment_from_oracle(
                        P0,
                        &format!("Suppression Field {i}"),
                        SUPPRESSION_FIELD,
                    );
                }
                Modifier::Unfloored(amount) => {
                    scenario
                        .add_creature(P0, &format!("Reducer {i}"), 1, 1)
                        .with_static_definition(unfloored_reducer(*amount));
                }
                Modifier::FlooredTwo(amount) => {
                    scenario
                        .add_creature(P0, &format!("Floored reducer {i}"), 1, 1)
                        .with_static_definition(reducer(*amount, Some(2)));
                }
                Modifier::Reducer { amount, floor } => {
                    scenario
                        .add_creature(P0, &format!("Reducer {i}"), 1, 1)
                        .with_static_definition(reducer(*amount, (*floor > 0).then_some(*floor)));
                }
            }
        }
        let source = match activator {
            Activator::Built { cost, rider } => {
                let mut ability = AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )
                .cost(cost);
                if let Some(amount_per) = rider {
                    ability.cost_reduction = Some(CostReduction {
                        mode: CostModifyMode::Reduce,
                        amount_per,
                        count: QuantityExpr::Fixed { value: 1 },
                        condition: None,
                    });
                }
                scenario
                    .add_creature(P0, "Activator", 2, 2)
                    .with_ability_definition(ability)
                    .id()
            }
            Activator::Oracle(text) => scenario
                .add_creature_from_oracle(P0, "Activator", 2, 2, text)
                .id(),
        };
        scenario.with_mana_pool(P0, colorless(pool));
        let mut runner = scenario.build();
        // Settle the layer system's derived per-object caches, which the first
        // action boundary would otherwise initialize, so a board snapshot
        // compares game state rather than cache warm-up.
        engine::game::layers::evaluate_layers(runner.state_mut());
        let ability_index = runner.state().objects[&source]
            .abilities
            .iter()
            .position(|a| matches!(a.kind, AbilityKind::Activated))
            .expect("the activator must have an activated ability");
        Self {
            runner,
            source,
            ability_index,
        }
    }

    fn state(&self) -> &GameState {
        self.runner.state()
    }

    fn pool(&self) -> usize {
        self.state().players[0].mana_pool.mana.len()
    }

    fn activate(&mut self) -> Result<ActionResult, engine::game::engine::EngineError> {
        self.runner.act(GameAction::ActivateAbility {
            source_id: self.source,
            ability_index: self.ability_index,
        })
    }

    fn on_stack(&self) -> bool {
        self.state()
            .stack
            .iter()
            .any(|entry| entry.source_id == self.source)
    }

    /// Activate and report the mana it spent, asserting it reached the stack.
    fn activate_and_pay(&mut self) -> usize {
        let before = self.pool();
        self.activate().expect("the activation must be legal");
        assert!(self.on_stack(), "the activation must reach the stack");
        before - self.pool()
    }

    fn prompt(
        &self,
    ) -> (
        &[CostReductionEntry],
        &[engine::types::casting_costs::CostReductionOutcome],
    ) {
        match &self.state().waiting_for {
            WaitingFor::OrderCostReductions {
                reductions,
                outcomes,
                ..
            } => (reductions, outcomes),
            other => panic!("expected the cost-reduction election, got {other:?}"),
        }
    }

    fn reductions(&self) -> Vec<CostReductionEntry> {
        self.prompt().0.to_vec()
    }

    /// The mana value each offered outcome locks in, cheapest first.
    fn outcome_totals(&self) -> Vec<u32> {
        self.prompt()
            .1
            .iter()
            .map(|outcome| outcome.locked_cost.mana_value())
            .collect()
    }

    /// Answer the election with the outcome at `index` (0 = cheapest).
    fn elect(&mut self, index: usize) -> Result<ActionResult, engine::game::engine::EngineError> {
        let order = self.prompt().1[index].order.clone();
        self.runner.act(GameAction::OrderCostReductions {
            order,
            hybrid_announcement: Vec::new(),
        })
    }
}

/// The paths at which two JSON values differ, for readable assertion failures.
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
fn assert_same_json(before: &serde_json::Value, after: &serde_json::Value, what: &str) {
    let mut diffs = Vec::new();
    json_diff("", before, after, &mut diffs);
    assert!(diffs.is_empty(), "{what}: {diffs:#?}");
}

/// The parts of the game an activation could touch before paying, as JSON.
fn board_snapshot(state: &GameState) -> serde_json::Value {
    serde_json::json!({
        "objects": serde_json::to_value(&state.objects).unwrap(),
        "players": serde_json::to_value(&state.players).unwrap(),
        "stack": serde_json::to_value(&state.stack).unwrap(),
        "battlefield": serde_json::to_value(&state.battlefield).unwrap(),
        "pending_activations": serde_json::to_value(&state.pending_activations).unwrap(),
        "lands_tapped_for_mana": serde_json::to_value(&state.lands_tapped_for_mana).unwrap(),
        "last_loop_action_sequence": serde_json::to_value(&state.last_loop_action_sequence).unwrap(),
    })
}

/// The whole game state as JSON, minus the fields a reversal legitimately moves:
/// the revision (the transport's staleness key) and the interaction authority
/// bound to the current decision.
fn state_without_revision(state: &GameState) -> serde_json::Value {
    let mut value = serde_json::to_value(state).unwrap();
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

fn assert_lock(
    snapshot: Option<&engine::types::casting_costs::ActivationCostSnapshot>,
    what: &str,
) {
    let snapshot = snapshot.unwrap_or_else(|| panic!("{what} must carry the cost snapshot"));
    assert!(
        matches!(
            snapshot.lock,
            ActivationCostLock::Locked {
                point: ActivationCostLockPoint::Announcement,
                order: Some(_),
            }
        ),
        "{what} must carry the ELECTED lock, got {:?}",
        snapshot.lock
    );
}

fn generic(n: u32) -> AbilityCost {
    AbilityCost::Mana {
        cost: ManaCost::generic(n),
    }
}

/// Reach guard for every Oracle-text fixture: the parse must yield the floor and
/// the raise these tests rely on, or a mis-parse would silently test nothing.
#[test]
fn the_printed_modifiers_parse_to_the_shapes_under_test() {
    let board = Board::new(
        &[Modifier::Grounds, Modifier::Suppression],
        generic(3),
        None,
        0,
    );
    let modes: Vec<StaticMode> = board
        .runner
        .state()
        .objects
        .values()
        .flat_map(|obj| {
            obj.static_definitions
                .as_slice()
                .iter()
                .map(|d| d.mode.clone())
        })
        .collect();
    assert!(
        modes.iter().any(|mode| matches!(
            mode,
            StaticMode::ReduceAbilityCost {
                mode: CostModifyMode::Reduce,
                amount: 2,
                minimum_mana: Some(1),
                ..
            }
        )),
        "Training Grounds must parse to a floored -2, got {modes:?}"
    );
    assert!(
        modes.iter().any(|mode| matches!(
            mode,
            StaticMode::ReduceAbilityCost {
                mode: CostModifyMode::Raise,
                amount: 2,
                exemption: ActivationExemption::ManaAbilities,
                ..
            }
        )),
        "Suppression Field must parse to a +2 raise exempting mana abilities, got {modes:?}"
    );
}

/// CR 601.2f: raises are added before reductions. Before this, application
/// followed battlefield order, so `{1}` under Suppression Field and Training
/// Grounds cost 1 or 3 depending on which entered first. 3 is not a total any
/// legal order produces: 1 + 2 = 3, and Training Grounds then takes it to 1.
#[test]
fn a_raise_is_applied_before_a_floored_reduction_in_either_battlefield_order() {
    for modifiers in [
        [Modifier::Suppression, Modifier::Grounds],
        [Modifier::Grounds, Modifier::Suppression],
    ] {
        let mut board = Board::new(&modifiers, generic(1), None, 5);
        assert_eq!(
            board.activate_and_pay(),
            1,
            "{modifiers:?}: {{1}} + {{2}} - {{2}} (floor one mana) must lock {{1}}"
        );
    }
}

/// CR 601.2f: the ability's own rider is one reduction among the others, not a
/// step that always runs first. It used to be hard-wired first, which is the more
/// expensive order here: `{3}` −2 then Training Grounds is `{1}`, while Training
/// Grounds then −2 is `{0}`. The floors differ, so the caster elects — and the
/// cheapest total is offered first.
#[test]
fn the_abilitys_own_rider_joins_the_election() {
    let mut board = Board::new(&[Modifier::Grounds], generic(3), Some(2), 5);
    board.activate().expect("the activation must be legal");
    assert_eq!(board.outcome_totals(), vec![0, 1]);
    assert!(
        board
            .reductions()
            .iter()
            .any(|entry| entry.provenance == ReductionProvenance::AbilityCostRider),
        "the rider must be one of the electable reductions"
    );
}

/// CR 601.2f: the election's outcomes do not depend on battlefield order, and
/// the cheapest — the default every preview uses — comes first.
#[test]
fn the_default_order_is_the_cheapest_in_either_battlefield_order() {
    for modifiers in [
        [Modifier::Grounds, Modifier::Unfloored(2)],
        [Modifier::Unfloored(2), Modifier::Grounds],
    ] {
        let mut board = Board::new(&modifiers, generic(3), None, 5);
        board.activate().expect("the activation must be legal");
        assert_eq!(board.outcome_totals(), vec![0, 1], "{modifiers:?}");
    }
}

/// Reductions with equal effective floors commute, so battlefield order changes
/// nothing — and they really did apply (each total is below the printed `{3}`).
#[test]
fn equal_floors_commute() {
    for modifiers in [
        [Modifier::Unfloored(1), Modifier::Unfloored(2)],
        [Modifier::Unfloored(2), Modifier::Unfloored(1)],
    ] {
        let mut board = Board::new(&modifiers, generic(3), None, 5);
        assert_eq!(board.activate_and_pay(), 0, "{modifiers:?}");
    }
    let mut board = Board::new(&[Modifier::Grounds, Modifier::Grounds], generic(3), None, 5);
    assert_eq!(
        board.activate_and_pay(),
        1,
        "two floored reductions stop at one mana"
    );
}

/// CR 606.1: a reduction cannot touch a bare loyalty cost, so a loyalty ability
/// under reducers keeps the mana-free fast path and pays only loyalty.
#[test]
fn a_bare_loyalty_cost_is_untouched_by_reductions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for amount in [1, 2] {
        let mut reducer = unfloored_reducer(amount);
        reducer.affected = None;
        if let StaticMode::ReduceAbilityCost { keyword, .. } = &mut reducer.mode {
            *keyword = "loyalty".to_string();
        }
        scenario
            .add_creature(P0, "Loyalty reducer", 1, 1)
            .with_static_definition(reducer);
    }
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, "+1: You gain 1 life.")
        .id();
    scenario.with_mana_pool(P0, colorless(5));
    let mut runner = scenario.build();
    let index = runner.state().objects[&walker]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("the planeswalker has a loyalty ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: index,
        })
        .expect("the loyalty ability must activate");
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        5,
        "no mana paid"
    );
    assert_eq!(runner.state().objects[&walker].loyalty, Some(4));
    assert_eq!(runner.state().stack.len(), 1);
}

/// The preview reads the default fold, which is the cheapest order, so an
/// activation that some electable order makes free is offered with no mana —
/// in the battlefield order that used to make it cost one.
#[test]
fn the_preview_offers_an_activation_the_cheapest_order_makes_free() {
    let board = Board::new(
        &[Modifier::Unfloored(2), Modifier::Grounds],
        generic(3),
        None,
        0,
    );
    assert!(can_activate_ability_now(
        board.runner.state(),
        P0,
        board.source,
        0
    ));
}

/// An ordinary `ActionResult`, frozen as it serialized before `disposition`
/// existed (captured at upstream `9168c9f87`'s shape). Every `Applied` result
/// must keep exactly these bytes; only a reversal adds the discriminator.
const FROZEN_V78_ACTION_RESULT: &str = r#"{"events":[{"type":"PriorityPassed","data":{"player_id":1}},{"type":"PhaseChanged","data":{"phase":"PostCombatMain"}}],"waiting_for":{"type":"Priority","data":{"player":0}}}"#;

#[test]
fn an_applied_action_result_is_byte_identical_to_v78_and_a_reversal_is_tagged() {
    use engine::types::events::GameEvent;
    use engine::types::game_state::{ActionDisposition, ActionResult, WaitingFor};
    use engine::types::player::PlayerId;

    let applied = ActionResult::applied(
        vec![
            GameEvent::PriorityPassed {
                player_id: PlayerId(1),
            },
            GameEvent::PhaseChanged {
                phase: Phase::PostCombatMain,
            },
        ],
        WaitingFor::Priority {
            player: PlayerId(0),
        },
    );
    assert_eq!(
        serde_json::to_string(&applied).unwrap(),
        FROZEN_V78_ACTION_RESULT
    );
    let parsed: ActionResult = serde_json::from_str(FROZEN_V78_ACTION_RESULT).unwrap();
    assert_eq!(parsed, applied);
    assert_eq!(parsed.disposition, ActionDisposition::Applied);

    let reversed = ActionResult::reversed(WaitingFor::Priority {
        player: PlayerId(0),
    });
    let encoded = serde_json::to_string(&reversed).unwrap();
    assert!(
        encoded.contains(r#""disposition":"Reversed""#),
        "a reversal must carry its discriminator, got {encoded}"
    );
    let decoded: ActionResult = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, reversed);
}

// ---------------------------------------------------------------------------
// The election itself (CR 601.2f + CR 602.2b)
// ---------------------------------------------------------------------------

/// The two orders of Training Grounds and an unfloored −2 on `{3}` lock `{0}`
/// and `{1}`, so the caster is asked — in BOTH battlefield orders, and with no
/// board change before the prompt: the lock precedes every mutation, and the
/// dry run that proves the activation legal ran on a discarded clone (its stack
/// placement, activation record and event never reach the real game). Electing
/// Training Grounds first is the `{0}` DIRECT PUSH — the path a lock on the
/// mana-payment step alone would miss; the other order pays `{1}`.
#[test]
fn the_caster_elects_the_order_before_anything_happens_including_the_zero_cost_push() {
    for modifiers in [
        [Modifier::Grounds, Modifier::Unfloored(2)],
        [Modifier::Unfloored(2), Modifier::Grounds],
    ] {
        for (choice, expected_paid) in [(0, 0), (1, 1)] {
            let mut board = Board::new(&modifiers, generic(3), None, 5);
            let before = board_snapshot(board.state());
            let result = board.activate().expect("the activation must be legal");
            assert!(
                result.events.is_empty(),
                "nothing may happen before the lock"
            );
            assert_same_json(
                &before,
                &board_snapshot(board.state()),
                &format!("{modifiers:?}: the board must be untouched at the prompt"),
            );
            assert_eq!(board.outcome_totals(), vec![0, 1], "{modifiers:?}");

            let pool = board.pool();
            let answer = board.elect(choice).expect("a legal election");
            assert_eq!(answer.disposition, ActionDisposition::Applied);
            assert!(
                board.on_stack(),
                "{modifiers:?}: the ability reaches the stack"
            );
            assert_eq!(
                pool - board.pool(),
                expected_paid,
                "{modifiers:?} choice {choice}"
            );
            assert!(
                matches!(board.state().waiting_for, WaitingFor::Priority { .. }),
                "no second prompt may follow, got {:?}",
                board.state().waiting_for
            );
        }
    }
}

/// CR 602.2 + CR 602.2b -> CR 601.2: an illegal activation is reversed as if it
/// never began, so it may not leave a prompt behind. Every order of these
/// reductions is observable, yet an ability with no legal target is refused
/// outright — the state is exactly as it was, and nothing was emitted.
#[test]
fn an_illegal_activation_never_reaches_the_election() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle("{3}: Destroy target artifact."),
        5,
    );
    let before = serde_json::to_value(board.state()).unwrap();
    let error = board.activate();
    assert!(error.is_err(), "no legal target: the activation is illegal");
    assert_eq!(serde_json::to_value(board.state()).unwrap(), before);
}

/// The same, for a modal ability whose every mode is target-illegal — a case the
/// legality preview's `mode_count > 0` check accepts and the activation refuses.
#[test]
fn a_modal_activation_with_no_legal_mode_never_reaches_the_election() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle(
            "{3}: Choose one —\n• Destroy target artifact.\n• Destroy target enchantment you don't control.",
        ),
        5,
    );
    assert!(
        board.state().objects[&board.source].abilities[board.ability_index]
            .modal
            .is_some(),
        "reach guard: the fixture must parse to a modal ability"
    );
    let before = serde_json::to_value(board.state()).unwrap();
    assert!(board.activate().is_err());
    assert_eq!(serde_json::to_value(board.state()).unwrap(), before);
}

/// Modal: the election precedes the mode choice, and the mode prompt carries
/// the elected total and the locked carrier.
#[test]
fn a_modal_activation_elects_before_choosing_modes() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle("{3}: Choose one —\n• You gain 1 life.\n• You gain 2 life."),
        5,
    );
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "the board at the prompt",
    );
    board.elect(1).expect("legal");
    match &board.state().waiting_for {
        WaitingFor::AbilityModeChoice {
            ability_cost,
            activation_cost_snapshot,
            ..
        } => {
            assert_eq!(
                ability_cost,
                &Some(generic(1)),
                "the mode prompt carries the elected total"
            );
            assert_lock(activation_cost_snapshot.as_deref(), "the mode prompt");
        }
        other => panic!("expected the mode choice, got {other:?}"),
    }
    let pool = board.pool();
    board
        .runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("a legal mode");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 1);
}

/// The lock a pending activation's carrier currently holds.
fn pending_lock(state: &GameState) -> ActivationCostLock {
    let pending = match &state.waiting_for {
        WaitingFor::ChooseXValue { pending_cast, .. }
        | WaitingFor::OrderCostReductions { pending_cast, .. } => pending_cast.as_ref(),
        _ => state
            .pending_cast
            .as_deref()
            .expect("an in-flight activation"),
    };
    pending
        .activation_cost_snapshot
        .as_ref()
        .expect("the carrier")
        .lock
        .clone()
}

/// CR 601.2b + CR 601.2f: a mana `{X}` is announced BEFORE the total cost is
/// determined, and Training Grounds counts "the mana in that cost" — the
/// announced X included. `{X}{1}` with Training Grounds: X=3 is `{4}` reduced to
/// `{2}`; X=0 is `{1}`, which the floor leaves at `{1}`.
#[test]
fn an_x_activation_is_priced_against_the_announced_x() {
    for (x, paid) in [(3, 2), (0, 1)] {
        let mut board = Board::with_activator(
            &[Modifier::Grounds],
            Activator::Oracle("{X}{1}: You gain X life."),
            6,
        );
        board.activate().expect("legal");
        assert!(
            matches!(board.state().waiting_for, WaitingFor::ChooseXValue { .. }),
            "X is announced first, got {:?}",
            board.state().waiting_for
        );
        assert_eq!(
            pending_lock(board.state()),
            ActivationCostLock::Open {
                point: ActivationCostLockPoint::XAnnounced
            },
            "a mana X defers the lock to its announcement"
        );
        let pool = board.pool();
        board
            .runner
            .act(GameAction::ChooseX { value: x })
            .expect("a legal X");
        assert!(board.on_stack());
        assert_eq!(pool - board.pool(), paid, "X={x}");
    }
}

/// `{X}`: the election follows the X announcement, against the concrete cost.
/// X=2 on `{X}{3}` is `{5}`: −2 (floor two) then −2 locks `{1}`; the reverse
/// locks `{2}`. The prompt survives a serde round trip at this point.
#[test]
fn an_x_activation_elects_after_announcing_x() {
    let mut board = Board::with_activator(
        &[Modifier::FlooredTwo(2), Modifier::Unfloored(2)],
        Activator::Oracle("{X}{3}: You gain X life."),
        6,
    );
    board.activate().expect("legal");
    let WaitingFor::ChooseXValue { .. } = board.state().waiting_for else {
        panic!(
            "expected the X announcement, got {:?}",
            board.state().waiting_for
        );
    };
    let before = board_snapshot(board.state());
    board
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("a legal X");
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "the board at the prompt",
    );
    assert_eq!(board.outcome_totals(), vec![1, 2]);
    assert_eq!(
        pending_lock(board.state()),
        ActivationCostLock::Open {
            point: ActivationCostLockPoint::XAnnounced
        }
    );
    let json = serde_json::to_string(board.state()).unwrap();
    let restored: GameState = serde_json::from_str(&json).unwrap();
    // `allows_cancel_cast` / `has_pending_cast` are derived at serialization,
    // not restored; everything else survives.
    let persisted = |mut value: serde_json::Value| {
        let object = value.as_object_mut().unwrap();
        object.remove("allows_cancel_cast");
        object.remove("has_pending_cast");
        value
    };
    assert_same_json(
        &persisted(serde_json::from_str(&json).unwrap()),
        &persisted(serde_json::to_value(&restored).unwrap()),
        "the restored prompt",
    );
    *board.runner.state_mut() = restored;

    let pool = board.pool();
    board.elect(1).expect("legal");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 2, "the elected {{2}}");
}

/// Control: a NON-mana X (remove X counters) changes no mana, so its lock stays
/// at announcement and the election precedes the X announcement.
#[test]
fn a_non_mana_x_activation_still_elects_at_announcement() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle("{3}, Remove X +1/+1 counters from this creature: You gain X life."),
        5,
    );
    let source = board.source;
    board
        .runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .counters
        .insert(engine::types::counter::CounterType::Plus1Plus1, 3);
    board.activate().expect("legal");
    assert_eq!(board.outcome_totals(), vec![0, 1]);
    assert_eq!(
        pending_lock(board.state()),
        ActivationCostLock::Open {
            point: ActivationCostLockPoint::Announcement
        }
    );
    board.elect(1).expect("legal");
    assert!(
        matches!(board.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "X is announced after the election, got {:?}",
        board.state().waiting_for
    );
    let pool = board.pool();
    board
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("a legal X");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 1, "the elected {{1}}");
}

/// CR 601.2b + CR 601.2f: reductions raise the X cap. `{X}{3}` with Training
/// Grounds and an unfloored −2, three mana: X=4 is `{7}` reduced to `{3}`.
/// Folding at announcement would lock `{X}` for a cap of 3.
#[test]
fn reductions_raise_the_x_cap_of_a_deferred_activation() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle("{X}{3}: You gain X life."),
        3,
    );
    board.activate().expect("legal");
    let WaitingFor::ChooseXValue {
        max,
        ref x_cost_previews,
        ..
    } = board.state().waiting_for
    else {
        panic!("expected the X announcement");
    };
    assert_eq!(max, 4);
    let preview = |x: u32| {
        x_cost_previews
            .iter()
            .find(|(value, _)| *value == x)
            .map(|(_, cost)| cost.mana_value())
    };
    assert_eq!(preview(0), Some(0));
    assert_eq!(preview(4), Some(3));
    board
        .runner
        .act(GameAction::ChooseX { value: 4 })
        .expect("the cap is affordable");
    assert!(board.on_stack());
    assert_eq!(board.pool(), 0);
}

/// CR 601.2h + CR 602.2b: an unpayable elected total at the X lock reverses the
/// activation. X precedes every cost on this route, so NOTHING was paid: the
/// board is the board from before the activation. (The activation was accepted
/// at `ActivateAbility`, so this is the `CancelCast`-from-`ChooseXValue`
/// reversal, not the pre-announcement one.)
#[test]
fn an_unpayable_election_at_the_x_lock_reverses_with_nothing_paid() {
    // X=0 on {X}{5}: −2 (floor two) then −3 locks {0}; the reverse locks {2}.
    let mut board = Board::with_activator(
        &[Modifier::FlooredTwo(2), Modifier::Unfloored(3)],
        Activator::Oracle("{X}{5}, {T}: You gain X life."),
        1,
    );
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    board
        .runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("a legal X");
    assert_eq!(board.outcome_totals(), vec![0, 2]);
    let result = board.elect(1).expect("a legal election is not an error");
    assert_eq!(result.disposition, ActionDisposition::Reversed);
    assert!(matches!(result.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(board.state().pending_cast.is_none());
    assert!(!board.state().objects[&board.source].tapped, "no tap paid");
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "after the reversal",
    );
}

/// CR 602.2a + CR 732.2a: an activation is accepted where its cost locks, so a
/// deferred X lock records its loop step at the lock — once — not at
/// `ActivateAbility`.
#[test]
fn an_x_lock_election_records_its_loop_step_exactly_once() {
    let mut board = loop_board(
        &[Modifier::FlooredTwo(2), Modifier::Unfloored(2)],
        "{X}{3}: Create X 1/1 white Soldier creature tokens.",
        6,
    );
    board.activate().expect("legal");
    assert!(
        board.state().last_loop_action_sequence.is_empty(),
        "not accepted before its cost locks"
    );
    board
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("a legal X");
    assert_eq!(board.outcome_totals(), vec![1, 2]);
    assert!(board.state().last_loop_action_sequence.is_empty());
    board.elect(0).expect("legal");
    assert!(board.on_stack());
    assert_eq!(board.state().last_loop_action_sequence.len(), 1);

    // Without an election the X lock itself accepts, once.
    let mut board = loop_board(
        &[Modifier::Grounds],
        "{X}{3}: Create X 1/1 white Soldier creature tokens.",
        6,
    );
    board.activate().expect("legal");
    assert!(board.state().last_loop_action_sequence.is_empty());
    board
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("a legal X");
    assert!(board.on_stack());
    assert_eq!(board.state().last_loop_action_sequence.len(), 1);
}

/// CR 601.2h + CR 733.1 + CR 733.2: the X-lock reversal reverses the ENTIRE
/// activation — including the bookkeeping of a real manual mana-ability
/// activation made before it and an accumulating loop period. The land's mana
/// stays undoable afterwards, the loop period is exactly as it was, and the
/// player has priority. The same holds for the player's own cancel at the X
/// prompt; the default order, by contrast, is accepted exactly once.
#[test]
fn an_x_lock_reversal_restores_the_mana_undo_window_and_the_loop_period() {
    const TEXT: &str = "{X}{5}: Create X 1/1 white Soldier creature tokens.";
    let build = || {
        let mut land = None;
        let mut board = Board::with_setup(
            &[Modifier::FlooredTwo(2), Modifier::Unfloored(3)],
            Activator::Oracle(TEXT),
            0,
            |scenario| {
                land = Some(scenario.add_basic_land(P0, engine::types::mana::ManaColor::White));
            },
        );
        let land = land.expect("the land");
        board.runner.state_mut().loop_detection = engine::types::game_state::LoopDetectionMode::On;
        // An accumulating loop period for this controller: an accepted
        // activation would APPEND to it.
        let card_id = board.state().objects[&board.source].card_id;
        board.runner.state_mut().last_loop_action_sequence =
            vec![engine::types::game_state::LoopActionContext {
                card_id,
                controller: P0,
                action: engine::types::game_state::LoopAction::Activate {
                    source_id: board.source,
                    ability_index: board.ability_index,
                },
                convoke: None,
                pins: Vec::new(),
            }];
        // A real manual mana-ability activation, through the engine-authored
        // selection.
        let (_, _, grouped) = engine::ai_support::legal_actions_full(board.state());
        let selection = grouped
            .get(&land)
            .into_iter()
            .flatten()
            .find_map(|action| match action {
                GameAction::TapLandForMana { selection } => Some(selection.clone()),
                _ => None,
            })
            .expect("the engine authors the land's mana selection");
        board
            .runner
            .act(GameAction::TapLandForMana { selection })
            .expect("tap the land");
        assert_eq!(
            board.state().lands_tapped_for_mana.get(&P0),
            Some(&vec![land]),
            "reach guard: the tap opened a mana-undo window"
        );
        (board, land)
    };

    // The elected {2} cannot be paid from the land's one mana: reversed.
    let (mut board, land) = build();
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    board
        .runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("a legal X");
    assert_eq!(board.outcome_totals(), vec![0, 2]);
    let result = board.elect(1).expect("a legal election is not an error");
    assert_eq!(result.disposition, ActionDisposition::Reversed);
    assert!(matches!(result.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "after the X-lock reversal",
    );
    board
        .runner
        .act(GameAction::UntapLandForMana { object_id: land })
        .expect("the mana-undo window survived the reversal");
    assert!(!board.state().objects[&land].tapped);

    // The player's own cancel at the X prompt reverses the same way.
    let (mut board, land) = build();
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    board.runner.act(GameAction::CancelCast).expect("cancel");
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "after cancelling at the X prompt",
    );
    board
        .runner
        .act(GameAction::UntapLandForMana { object_id: land })
        .expect("the mana-undo window survived the cancel");

    // Control: the default order is accepted, exactly once.
    let (mut board, _) = build();
    let steps = board.state().last_loop_action_sequence.len();
    board.activate().expect("legal");
    board
        .runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("a legal X");
    let result = board.elect(0).expect("legal");
    assert_eq!(result.disposition, ActionDisposition::Applied);
    assert!(board.on_stack());
    assert_eq!(board.state().last_loop_action_sequence.len(), steps + 1);
    assert!(
        !board.state().lands_tapped_for_mana.contains_key(&P0),
        "the accepted activation closed the mana-undo window"
    );
}

/// CR 601.2b: a modal mana-`{X}` activation announces X with its modes — before
/// targets and before any cost (CR 601.2c, CR 601.2h) — so its X is priced and
/// its discard paid only after X. `{X}{3}, Discard a card` with Training
/// Grounds, X=2: `{5}` reduced to `{3}`. (X used to be skipped on this route,
/// the discard paid first and X silently 0.)
#[test]
fn a_modal_x_activation_announces_x_before_targets_and_costs() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds],
        Activator::Oracle(
            "{X}{3}, Discard a card: Choose one —\n• You gain X life.\n• This creature deals 1 damage to any target.",
        ),
        5,
    );
    let card = {
        let state = board.runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(8_801),
            P0,
            "Discard Fodder".to_string(),
            engine::types::zones::Zone::Hand,
        );
        engine::game::layers::evaluate_layers(state);
        id
    };
    board.activate().expect("legal");
    board
        .runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("a legal mode");
    assert!(
        matches!(board.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "X follows the modes, got {:?}",
        board.state().waiting_for
    );
    board
        .runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("a legal X");
    assert!(
        matches!(
            board.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "targets follow X, got {:?}",
        board.state().waiting_for
    );
    let pool = board.pool();
    board
        .runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Player(P1)],
        })
        .expect("a legal target");
    board
        .runner
        .act(GameAction::SelectCards { cards: vec![card] })
        .expect("a legal discard");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 3);
    assert!(
        board.state().players[0].hand.is_empty(),
        "the discard was paid"
    );
}

/// Targets: the election precedes target selection, and the target prompt's
/// pending activation carries the locked carrier.
#[test]
fn a_targeted_activation_elects_before_choosing_targets() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle("{3}: This creature deals 1 damage to any target."),
        5,
    );
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "the board at the prompt",
    );
    board.elect(1).expect("legal");
    let WaitingFor::TargetSelection { pending_cast, .. } = &board.state().waiting_for else {
        panic!(
            "expected target selection, got {:?}",
            board.state().waiting_for
        );
    };
    assert_lock(
        pending_cast.activation_cost_snapshot.as_deref(),
        "the target prompt",
    );
    let pool = board.pool();
    board
        .runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Player(P1)],
        })
        .expect("a legal target");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 1);
}

/// Interactive costs: the election precedes the discard prompt.
#[test]
fn an_interactive_cost_activation_elects_before_paying_any_cost() {
    let mut board = Board::with_activator(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        Activator::Oracle("{3}, Discard a card: You gain 1 life."),
        5,
    );
    let card = {
        let state = board.runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(8_800),
            P0,
            "Discard Fodder".to_string(),
            engine::types::zones::Zone::Hand,
        );
        engine::game::layers::evaluate_layers(state);
        id
    };
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "the board at the prompt",
    );
    board.elect(1).expect("legal");
    let WaitingFor::PayCost { resume, .. } = &board.state().waiting_for else {
        panic!(
            "expected the discard prompt, got {:?}",
            board.state().waiting_for
        );
    };
    let engine::types::game_state::CostResume::Spell { spell } = resume else {
        panic!("expected an activation resume, got {resume:?}");
    };
    assert_lock(
        spell.activation_cost_snapshot.as_deref(),
        "the discard prompt",
    );
    let pool = board.pool();
    board
        .runner
        .act(GameAction::SelectCards { cards: vec![card] })
        .expect("a legal discard");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 1);
}

/// Cancelling the election reverses an activation that never began paying.
#[test]
fn cancelling_the_election_returns_to_priority_untouched() {
    let mut board = Board::new(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        generic(3),
        None,
        5,
    );
    let before = board_snapshot(board.state());
    board.activate().expect("legal");
    board.runner.act(GameAction::CancelCast).expect("cancel");
    assert!(matches!(
        board.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_same_json(
        &before,
        &board_snapshot(board.state()),
        "the board at the prompt",
    );
    assert!(board.state().pending_cast.is_none());
}

/// A malformed order is rejected and the prompt stays live.
#[test]
fn a_malformed_order_is_rejected_and_the_prompt_survives() {
    let mut board = Board::new(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        generic(3),
        None,
        5,
    );
    board.activate().expect("legal");
    let rejected = board.runner.act(GameAction::OrderCostReductions {
        order: vec![0, 0],
        hybrid_announcement: Vec::new(),
    });
    assert!(rejected.is_err());
    assert_eq!(
        board.outcome_totals(),
        vec![0, 1],
        "the prompt is still live"
    );
}

/// The AI answers from the engine's own representatives, cheapest first.
#[test]
fn the_ai_candidates_are_the_engine_outcomes() {
    let mut board = Board::new(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        generic(3),
        None,
        5,
    );
    board.activate().expect("legal");
    let expected: Vec<Vec<usize>> = board.prompt().1.iter().map(|o| o.order.clone()).collect();
    let offered: Vec<Vec<usize>> = engine::ai_support::legal_actions(board.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::OrderCostReductions { order, .. } => Some(order),
            _ => None,
        })
        .collect();
    assert_eq!(offered, expected);
}

/// The paused activation survives a full serialize / deserialize, and the
/// deserialized game answers it exactly as the live one would.
#[test]
fn the_paused_activation_round_trips_through_serde() {
    let mut board = Board::new(
        &[Modifier::Grounds, Modifier::Unfloored(2)],
        generic(3),
        None,
        5,
    );
    board.activate().expect("legal");
    let json = serde_json::to_string(board.state()).unwrap();
    let restored: GameState = serde_json::from_str(&json).unwrap();
    *board.runner.state_mut() = restored;
    let pool = board.pool();
    board.elect(1).expect("the restored prompt answers");
    assert!(board.on_stack());
    assert_eq!(pool - board.pool(), 1);
}

/// CR 601.2h + CR 602.2b: an elected total that cannot be paid reverses the
/// activation. The action boundary restores the state from before the answer —
/// which, because nothing happened before the prompt, is the state from before
/// the activation — and applies only the return to priority: no events, no log,
/// and exactly one revision bump so clients drop the prompt. The undo window for
/// a manually tapped land survives, although the resume cleared it before
/// failing — the restore, not the handler, undoes that.
#[test]
fn an_unpayable_election_reverses_the_activation_completely() {
    // −2 (floor two) then −3 on {5} locks {0}; the reverse locks {2}.
    let mut board = Board::new(
        &[Modifier::FlooredTwo(2), Modifier::Unfloored(3)],
        generic(5),
        None,
        1,
    );
    let land = board
        .state()
        .objects
        .keys()
        .copied()
        .next()
        .expect("some object stands in for the tapped land");
    board
        .runner
        .state_mut()
        .lands_tapped_for_mana
        .insert(P0, vec![land]);
    let pre_activation = state_without_revision(board.state());

    board.activate().expect("legal: the default order is free");
    let prompt_revision = board.state().state_revision;
    assert_eq!(board.outcome_totals(), vec![0, 2]);

    let result = board.elect(1).expect("a legal election is not an error");
    assert_eq!(result.disposition, ActionDisposition::Reversed);
    assert!(result.events.is_empty());
    assert!(result.log_entries.is_empty());
    assert!(matches!(result.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(board.state().state_revision, prompt_revision + 1);
    assert_same_json(
        &pre_activation,
        &state_without_revision(board.state()),
        "after the reversal",
    );

    // Control: the free order on the same board is an ordinary success.
    let mut board = Board::new(
        &[Modifier::FlooredTwo(2), Modifier::Unfloored(3)],
        generic(5),
        None,
        1,
    );
    board.activate().expect("legal");
    let result = board.elect(0).expect("legal");
    assert_eq!(result.disposition, ActionDisposition::Applied);
    assert!(board.on_stack());
}

// ---------------------------------------------------------------------------
// The acceptance and placement authorities
// ---------------------------------------------------------------------------

const TOKEN_MAKER: &str = "{3}: Create a 1/1 white Soldier creature token.";

fn loop_board(modifiers: &[Modifier], text: &'static str, pool: usize) -> Board {
    let mut board = Board::with_activator(modifiers, Activator::Oracle(text), pool);
    board.runner.state_mut().loop_detection = engine::types::game_state::LoopDetectionMode::On;
    board
}

/// CR 602.2a + CR 732.2a: loop accounting records an ACCEPTED activation. The
/// election prompt accepts nothing, so it records nothing; the resumed
/// activation records exactly once.
#[test]
fn an_elected_activation_seeds_its_loop_step_once_after_the_election() {
    let mut board = loop_board(&[Modifier::Grounds, Modifier::Unfloored(2)], TOKEN_MAKER, 5);
    board.activate().expect("legal");
    assert!(
        board.state().last_loop_action_sequence.is_empty(),
        "the prompt accepted nothing"
    );
    board.elect(1).expect("legal");
    assert_eq!(
        board.state().last_loop_action_sequence.len(),
        1,
        "the resumed token-making activation seeds exactly one loop step"
    );
}

/// A continuing loop period for the same controller gains one step, not two.
#[test]
fn an_elected_activation_appends_its_loop_step_exactly_once() {
    let mut board = loop_board(&[Modifier::Grounds, Modifier::Unfloored(2)], TOKEN_MAKER, 5);
    let card_id = board.state().objects[&board.source].card_id;
    board.runner.state_mut().last_loop_action_sequence =
        vec![engine::types::game_state::LoopActionContext {
            card_id,
            controller: P0,
            action: engine::types::game_state::LoopAction::Activate {
                source_id: board.source,
                ability_index: board.ability_index,
            },
            convoke: None,
            pins: Vec::new(),
        }];
    board.activate().expect("legal");
    assert_eq!(board.state().last_loop_action_sequence.len(), 1);
    board.elect(0).expect("legal");
    assert_eq!(board.state().last_loop_action_sequence.len(), 2);
}

/// A reversal leaves the loop period exactly as it was.
#[test]
fn a_reversed_activation_records_no_loop_step() {
    let mut board = loop_board(
        &[Modifier::FlooredTwo(2), Modifier::Unfloored(3)],
        "{5}: Create a 1/1 white Soldier creature token.",
        1,
    );
    board.activate().expect("legal");
    let result = board.elect(1).expect("a legal election");
    assert_eq!(result.disposition, ActionDisposition::Reversed);
    assert!(board.state().last_loop_action_sequence.is_empty());
}

/// The placement authority names the entry its caller just pushed, even when an
/// earlier activation of the SAME ability of the SAME permanent is already on
/// the stack (in debug builds the authority asserts this). One `AbilityActivated`
/// event and one activation record per placement, at each placement site.
#[test]
fn each_placement_names_its_own_entry_over_an_earlier_identical_activation() {
    // Site 1 — the direct push: a mana-free cost paid synchronously.
    let mut board = Board::new(
        &[],
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        },
        None,
        0,
    );
    for expected in 1..=2 {
        board.activate().expect("legal");
        assert_eq!(
            board.state().stack.len(),
            expected,
            "direct push #{expected}"
        );
    }
    assert_eq!(
        board.state().pending_activations.len(),
        2,
        "one activation record per placement"
    );

    // Site 2 — `push_activated_ability_to_stack`: a mana cost paid automatically.
    let mut board = Board::new(&[], generic(1), None, 2);
    for expected in 1..=2 {
        board.activate().expect("legal");
        assert_eq!(
            board.state().stack.len(),
            expected,
            "mana-leg push #{expected}"
        );
    }
    assert_eq!(board.state().pending_activations.len(), 2);
}

/// Site 3 — the loyalty tail pushes directly, not through the shared entry
/// builder, so it calls the placement authority itself. The hostile case (an
/// earlier activation of the same ability still on the stack) cannot arise here:
/// CR 606.3 lets a loyalty ability be activated only while the stack is empty.
/// So this pins the ordinary placement: one entry, one `AbilityActivated`, one
/// activation record.
#[test]
fn the_loyalty_placement_records_through_the_authority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Test Walker", "Test", 3, "+1: You gain 1 life.")
        .id();
    let mut runner = scenario.build();
    let index = runner.state().objects[&walker]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .unwrap();
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index: index,
        })
        .expect("the loyalty ability activates");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                engine::types::events::GameEvent::AbilityActivated { .. }
            ))
            .count(),
        1
    );
    assert_eq!(runner.state().pending_activations, vec![(walker, index)]);
}

/// CR 601.2f "in any order": past eight reductions the election must still find
/// every total. With these nine reducers on `{6}`, every ROTATION of the
/// battlefield order locks `{0}`, but another order locks `{1}` — a sampled plan
/// sees one total and would suppress the prompt, taking the `{1}` order away.
/// The exact search offers both, and each elects as offered.
#[test]
fn nine_reductions_offer_a_total_only_a_non_rotation_order_reaches() {
    let nine = [
        (1, 0),
        (1, 1),
        (3, 2),
        (2, 2),
        (2, 0),
        (1, 2),
        (1, 1),
        (1, 2),
        (2, 0),
    ]
    .map(|(amount, floor)| Modifier::Reducer { amount, floor });
    for (choice, expected_paid) in [(0, 0), (1, 1)] {
        let mut board = Board::new(&nine, generic(6), None, 6);
        board.activate().expect("legal");
        assert_eq!(board.reductions().len(), 9, "reach guard: all nine apply");
        assert_eq!(board.outcome_totals(), vec![0, 1]);
        let pool = board.pool();
        board.elect(choice).expect("legal");
        assert!(board.on_stack());
        assert_eq!(pool - board.pool(), expected_paid, "choice {choice}");
    }
}

/// CR 601.2f "in any order", on the proven closed form past the old
/// sixteen-reducer bound: seventeen reducers with floors 0 and 1 on `{12}`. The
/// default order and every rotation of the battlefield order lock `{0}`; firing
/// every floor-0 reducer first locks `{1}`. A sampled plan would publish `{0}`
/// alone as the whole menu; the closed form offers both.
#[test]
fn seventeen_reductions_offer_a_total_no_sampled_order_reaches() {
    let seventeen = [
        (1, 0),
        (1, 1),
        (2, 0),
        (2, 1),
        (1, 0),
        (2, 0),
        (2, 1),
        (1, 1),
        (1, 1),
        (2, 1),
        (2, 1),
        (1, 1),
        (2, 0),
        (1, 1),
        (1, 1),
        (2, 0),
        (2, 1),
    ]
    .map(|(amount, floor)| Modifier::Reducer { amount, floor });
    for (choice, expected_paid) in [(0, 0), (1, 1)] {
        let mut board = Board::new(&seventeen, generic(12), None, 12);
        board.activate().expect("legal");
        assert_eq!(
            board.reductions().len(),
            17,
            "reach guard: all seventeen apply"
        );
        assert_eq!(board.outcome_totals(), vec![0, 1]);
        let pool = board.pool();
        board.elect(choice).expect("legal");
        assert!(board.on_stack());
        assert_eq!(pool - board.pool(), expected_paid, "choice {choice}");
    }
}

//! The per-turn "added mana with this ability this turn" ledger
//! (`GameState::triggered_abilities_added_mana_this_turn`).
//!
//! CR 603.4 + CR 607.1c: an intervening-if about whether a triggered ability
//! added mana "with this ability" needs an exact, per-occurrence and per-player
//! record (CR 113.2c per ability, CR 400.7 per incarnation) of the triggered
//! abilities that actually put mana into a pool this turn. These rows pin the
//! ledger's writers, its non-writers, its turn reset, its loop-projection
//! class and its deterministic serialization. Its reader is the intervening-if
//! leaf `TriggerCondition::AddedManaWithThisAbilityThisTurn`, which the Carpet
//! of Flowers rows drive through both main phases of a turn.
//!
//! Fixtures. Real printings are used verbatim (Oracle text re-checked against
//! Scryfall by exact name): Carpet of Flowers, Return the Favor, Mana Flare,
//! Dark Ritual, Braid of Fire and Solemnity. The remaining fixtures are
//! SYNTHETIC lines chosen to
//! reach one deposit route each:
//! - `UPKEEP_ADD_GREEN` (synthetic): a phase trigger that adds fixed mana and
//!   therefore uses the stack (CR 605.5a).
//! - `MAIN_PHASE_ADD_X` (synthetic): Carpet of Flowers with its "if you
//!   haven't added mana with this ability this turn" guard removed, so these
//!   rows observe the ledger's writes independently of the guard that reads it.
//!   It reaches the optional (CR 603.5) and color-choice prompt routes.
//! - `TARGETED_ACTIVATED_ADD_GREEN` (synthetic): an activated ability with a
//!   target, so it is not a mana ability (CR 605.1a) and resolves through the
//!   same `Effect::Mana` resolver from the stack.
//! - `ENTERS_ADD_RED` (synthetic): an ETB trigger whose object can be blinked
//!   to mint a fresh incarnation.

use std::collections::HashSet;

use engine::analysis::resource::loop_states_equal_modulo_resources;
use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ControllerRef, Effect, ManaContribution, ManaProduction,
    ManaTargetRole, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef,
    TriggerCondition, TriggerDefinitionRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, ManaChoice, ManaChoiceContext, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Synthetic: a stack-resolved phase trigger that adds fixed mana.
const UPKEEP_ADD_GREEN: &str = "At the beginning of your upkeep, add {G}.";
/// Carpet of Flowers, verbatim (Scryfall, exact name).
const CARPET_OF_FLOWERS: &str = "At the beginning of each of your main phases, if you haven't added mana with this ability this turn, you may add X mana of any one color, where X is the number of Islands target opponent controls.";
/// Return the Favor, verbatim (local MTGJSON AtomicCards).
const RETURN_THE_FAVOR: &str = "Spree (Choose one or more additional costs.)\n\
+ {1} — Copy target instant spell, sorcery spell, activated ability, or triggered ability. You may choose new targets for the copy.\n\
+ {1} — Change the target of target spell or ability with a single target.";
/// Synthetic: Carpet of Flowers without its intervening-if guard.
const MAIN_PHASE_ADD_X: &str = "At the beginning of each of your main phases, you may add X mana of any one color, where X is the number of Islands target opponent controls.";
/// Synthetic: a targeted activated `Effect::Mana` (not a mana ability).
const TARGETED_ACTIVATED_ADD_GREEN: &str = "{T}: Target player adds {G}.";
/// Synthetic: an ETB trigger that adds fixed mana.
const ENTERS_ADD_RED: &str = "When this creature enters, add {R}{R}{R}.";
const MANA_FLARE: &str = "Whenever a player taps a land for mana, that player adds one mana of any type that land produced.";
const DARK_RITUAL: &str = "Add {B}{B}{B}.";
const BRAID_OF_FIRE: &str = "Cumulative upkeep—Add {R}. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)";
const SOLEMNITY: &str =
    "Players can't get counters.\nCounters can't be put on artifacts, creatures, enchantments, or lands.";

/// Upper bound on driver actions; every row finishes far below it, so hitting
/// it means the pipeline stalled rather than that the row needs more steps.
const MAX_DRIVER_STEPS: usize = 200;

/// Library padding so rows that cross a draw step never lose to an empty draw.
const LIBRARY_PADDING: usize = 5;

/// How the bounded driver answers the prompts it is allowed to answer.
struct Answers {
    accept_optional: bool,
    /// Per-source answers to the optional prompt, overriding `accept_optional`.
    optional_overrides: Vec<(ObjectId, bool)>,
    target_player: PlayerId,
}

impl Answers {
    fn accepts_optional_from(&self, source: ObjectId) -> bool {
        self.optional_overrides
            .iter()
            .find(|(overridden, _)| *overridden == source)
            .map_or(self.accept_optional, |(_, accept)| *accept)
    }
}

const ACCEPT_TARGETING_OPPONENT: Answers = Answers {
    accept_optional: true,
    optional_overrides: Vec::new(),
    target_player: P1,
};

const DECLINE_TARGETING_OPPONENT: Answers = Answers {
    accept_optional: false,
    optional_overrides: Vec::new(),
    target_player: P1,
};

/// P0's mana pool as the driver read it before one of its actions.
struct PoolReading {
    phase: Phase,
    green: usize,
    total: usize,
}

/// Everything the driver observed on the way, for reach guards.
#[derive(Default)]
struct Trace {
    /// Every distinct `(phase, source, trigger_definition_ref)` seen on the
    /// stack, in first-seen order.
    stack_entries: Vec<(Phase, ObjectId, Option<TriggerDefinitionRef>)>,
    /// Each optional prompt answered: its phase, its source and the answer.
    optional_answers: Vec<(Phase, ObjectId, bool)>,
    /// The ref carried by each `ManaChoiceContext::ResolvingEffect` prompt answered.
    resolving_mana_choice_refs: Vec<Option<TriggerDefinitionRef>>,
    /// P0's pool before every driver action. CR 106.4 empties pools between
    /// steps and phases, so a deposit is visible only inside its own phase.
    p0_pool: Vec<PoolReading>,
}

impl Trace {
    fn saw_entry_from(&self, source: ObjectId) -> bool {
        self.stack_entries
            .iter()
            .any(|(_, entry_source, _)| *entry_source == source)
    }

    fn saw_entry_from_in(&self, source: ObjectId, phase: Phase) -> bool {
        !self.refs_from_in(source, phase).is_empty()
    }

    /// The `trigger_definition_ref` of the first stack entry from `source`.
    fn first_ref_from(&self, source: ObjectId) -> TriggerDefinitionRef {
        self.stack_entries
            .iter()
            .find(|(_, entry_source, _)| *entry_source == source)
            .and_then(|(_, _, definition_ref)| definition_ref.clone())
            .unwrap_or_else(|| panic!("no triggered stack entry from {source:?} was observed"))
    }

    /// The refs of every distinct stack entry from `source` seen in `phase`.
    fn refs_from_in(&self, source: ObjectId, phase: Phase) -> Vec<Option<TriggerDefinitionRef>> {
        self.stack_entries
            .iter()
            .filter(|(entry_phase, entry_source, _)| {
                *entry_phase == phase && *entry_source == source
            })
            .map(|(_, _, definition_ref)| definition_ref.clone())
            .collect()
    }

    /// The single `Some` ref seen from `source` in `phase`; panics otherwise.
    fn only_ref_from_in(&self, source: ObjectId, phase: Phase) -> TriggerDefinitionRef {
        match self.refs_from_in(source, phase).as_slice() {
            [Some(definition_ref)] => definition_ref.clone(),
            other => {
                panic!("expected one ref-bearing entry from {source:?} in {phase:?}, saw {other:?}")
            }
        }
    }

    fn pool_readings_in(&self, phase: Phase) -> impl Iterator<Item = &PoolReading> {
        self.p0_pool
            .iter()
            .filter(move |reading| reading.phase == phase)
    }

    /// The most green mana P0's pool held at any driver step in `phase`.
    fn max_green_in(&self, phase: Phase) -> Option<usize> {
        self.pool_readings_in(phase)
            .map(|reading| reading.green)
            .max()
    }
}

/// Drive the real turn/priority pipeline until `stop` holds. Answers priority,
/// optional, color-choice, targeting, trigger-ordering and attack prompts;
/// panics on any other prompt (including `UnlessPayment`, which rows answer
/// themselves).
fn drive_until(
    runner: &mut GameRunner,
    answers: &Answers,
    trace: &mut Trace,
    stop: impl Fn(&GameState, &Trace) -> bool,
) {
    for _ in 0..MAX_DRIVER_STEPS {
        let state = runner.state();
        for entry in &state.stack {
            let definition_ref = entry
                .ability()
                .and_then(|ability| ability.trigger_definition_ref.clone());
            let observed = (state.phase, entry.source_id, definition_ref);
            if !trace.stack_entries.contains(&observed) {
                trace.stack_entries.push(observed);
            }
        }
        let p0_pool = &state.players[P0.0 as usize].mana_pool;
        trace.p0_pool.push(PoolReading {
            phase: state.phase,
            green: p0_pool.count_color(ManaType::Green),
            total: p0_pool.mana.len(),
        });
        if stop(state, trace) {
            return;
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                let accept = answers.accepts_optional_from(*source_id);
                trace
                    .optional_answers
                    .push((state.phase, *source_id, accept));
                GameAction::DecideOptionalEffect { accept }
            }
            // CR 603.3b: the order is the controller's choice; the identity
            // permutation is one legal answer.
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::ChooseManaColor { context, .. } => {
                if let ManaChoiceContext::ResolvingEffect(ability) = context {
                    trace
                        .resolving_mana_choice_refs
                        .push(ability.trigger_definition_ref.clone());
                }
                GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                }
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(answers.target_player)),
                }
            }
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            other => panic!("driver met a prompt it does not answer: {other:?}"),
        };
        runner
            .act(action)
            .expect("driver action must be accepted by the engine");
    }
    panic!("driver exceeded {MAX_DRIVER_STEPS} actions without reaching its stop condition");
}

fn pad_libraries(scenario: &mut GameScenario) {
    for _ in 0..LIBRARY_PADDING {
        scenario.add_land_to_library_top(P0, "Forest");
        scenario.add_land_to_library_top(P1, "Forest");
    }
}

fn ledger(runner: &GameRunner) -> &HashSet<(TriggerDefinitionRef, PlayerId)> {
    &runner.state().triggered_abilities_added_mana_this_turn
}

fn pool_count(runner: &GameRunner, player: PlayerId, mana_type: ManaType) -> usize {
    runner.state().players[player.0 as usize]
        .mana_pool
        .count_color(mana_type)
}

fn pool_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize]
        .mana_pool
        .mana
        .len()
}

/// Stop once an entry from `source` has been seen and the stack has drained
/// back to a priority window.
fn resolved_entry_from(source: ObjectId) -> impl Fn(&GameState, &Trace) -> bool {
    move |state, trace| {
        trace.saw_entry_from(source)
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    }
}

/// Stop once an entry from `source` has been seen in `phase` and, still inside
/// that phase, the stack has drained back to a priority window.
fn resolved_entry_from_in(source: ObjectId, phase: Phase) -> impl Fn(&GameState, &Trace) -> bool {
    move |state, trace| {
        trace.saw_entry_from_in(source, phase)
            && state.phase == phase
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
    }
}

/// Stop as soon as the game reaches `phase`.
fn reached(phase: Phase) -> impl Fn(&GameState, &Trace) -> bool {
    move |state, _| state.phase == phase
}

/// P0 controls `carpet_count` copies of Carpet of Flowers and P1 controls
/// `island_count` Islands; the game starts at P0's untap step.
fn carpet_runner(carpet_count: usize, island_count: usize) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let carpets = (0..carpet_count)
        .map(|_| {
            scenario
                .add_enchantment_from_oracle(P0, "Carpet of Flowers", CARPET_OF_FLOWERS)
                .id()
        })
        .collect();
    for _ in 0..island_count {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    pad_libraries(&mut scenario);
    (scenario.build(), carpets)
}

/// CR 603.4 + CR 707.10: Reach a real copied Carpet trigger using Return the
/// Favor. The original targets P1; the copy controller may retarget it to P0.
fn carpet_copy_at_retarget(
    copy_controller: PlayerId,
    p0_islands: usize,
    p1_islands: usize,
) -> (GameRunner, ObjectId, ObjectId, TriggerDefinitionRef) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let carpet = scenario
        .add_enchantment_from_oracle(P0, "Carpet of Flowers", CARPET_OF_FLOWERS)
        .id();
    let favor = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            copy_controller,
            "Return the Favor",
            true,
            RETURN_THE_FAVOR,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        });
        builder.id()
    };
    for _ in 0..3 {
        scenario.add_basic_land(copy_controller, ManaColor::Red);
    }
    for _ in 0..p0_islands {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..p1_islands {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        |state, _| {
            state.phase == Phase::PreCombatMain
                && state.stack.len() == 1
                && state.stack[0].source_id == carpet
                && matches!(state.waiting_for, WaitingFor::Priority { player } if player == copy_controller)
        },
    );
    let original = &runner.state().stack[0];
    let original_id = original.id;
    let definition = original
        .ability()
        .and_then(|ability| ability.trigger_definition_ref.clone())
        .expect("natural Carpet trigger carries its definition");
    assert_eq!(original.controller, P0);
    assert!(ledger(&runner).is_empty());

    let copy_id = {
        let outcome = runner
            .cast(favor)
            .modes(&[0])
            .target_object(original_id)
            .resolve();
        let WaitingFor::CopyRetarget {
            player,
            copy_id,
            target_slots,
            ..
        } = outcome.final_waiting_for()
        else {
            panic!("Return the Favor must reach CopyRetarget");
        };
        assert_eq!(*player, copy_controller);
        assert_eq!(target_slots.len(), 1);
        assert_eq!(target_slots[0].current, Some(TargetRef::Player(P1)));
        *copy_id
    };
    (runner, original_id, copy_id, definition)
}

/// CR 109.5 + CR 603.4 + CR 707.10b: P1's copy may add mana with the same
/// ability while P0's original remains open for P0.
#[test]
fn carpet_copy_by_opponent_does_not_suppress_original() {
    let (mut runner, original_id, copy_id, definition) = carpet_copy_at_retarget(P1, 2, 3);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("P1 retargets the copy to P0");
    let copy = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .expect("copy remains on the stack");
    assert_eq!(copy.controller, P1);
    assert_eq!(
        copy.ability()
            .and_then(|ability| ability.trigger_definition_ref.as_ref()),
        Some(&definition)
    );
    assert_ne!(copy_id, original_id);

    let mut copy_trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut copy_trace,
        |state, _| {
            state.stack.len() == 1
                && state.stack[0].id == original_id
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(pool_count(&runner, P1, ManaType::Green), 2);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 0);
    assert_eq!(ledger(&runner), &HashSet::from([(definition.clone(), P1)]));

    let projected = serde_json::to_value(ClientGameStateRef::wrap(runner.state(), Some(P0)))
        .expect("paused state projects to the client");
    let pair = serde_json::to_value((definition.clone(), P1)).expect("pair serializes");
    assert!(
        projected["state"]["triggered_abilities_added_mana_this_turn"]
            .as_array()
            .expect("projected ledger")
            .contains(&pair)
    );
    assert!(projected["state"]["stack"]
        .to_string()
        .contains("AddedManaWithThisAbilityThisTurn"));

    let mut original_trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut original_trace,
        |state, _| matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut original_trace,
        |state, _| {
            state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(
        ledger(&runner),
        &HashSet::from([(definition.clone(), P1), (definition, P0)])
    );
}

/// CR 603.4 + CR 707.10b: a same-controller copy shares the same
/// definition/player history, so its successful add closes the original.
#[test]
fn carpet_copy_by_controller_suppresses_original_after_positive_deposit() {
    let (mut runner, original_id, copy_id, definition) = carpet_copy_at_retarget(P0, 0, 3);
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("P0 retains P1 as the copy's target");
    let copy = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .unwrap();
    assert_eq!(copy.controller, P0);
    assert_eq!(
        copy.ability()
            .and_then(|ability| ability.trigger_definition_ref.as_ref()),
        Some(&definition)
    );
    let mut trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        |state, _| {
            state.stack.len() == 1
                && state.stack[0].id == original_id
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(ledger(&runner), &HashSet::from([(definition.clone(), P0)]));
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        |state, _| {
            state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    let carpet = definition.source.object_id;
    assert_eq!(ledger(&runner), &HashSet::from([(definition, P0)]));
    assert_eq!(trace.optional_answers.len(), 1);
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        reached(Phase::End),
    );
    assert!(trace.refs_from_in(carpet, Phase::PostCombatMain).is_empty());
}

/// CR 603.5 + CR 603.4: declining the copy adds nothing for either
/// controller, leaving the original's resolution check open.
#[test]
fn declined_carpet_copy_leaves_original_open() {
    for copy_controller in [P0, P1] {
        let (mut runner, original_id, copy_id, definition) =
            carpet_copy_at_retarget(copy_controller, 2, 3);
        if copy_controller == P1 {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .expect("P1 retargets its copy");
        } else {
            runner
                .act(GameAction::KeepAllCopyTargets)
                .expect("P0 keeps the original target");
        }
        let copy = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap();
        assert_eq!(copy.controller, copy_controller);
        assert_eq!(
            copy.ability()
                .and_then(|ability| ability.trigger_definition_ref.as_ref()),
            Some(&definition)
        );
        let mut copy_trace = Trace::default();
        drive_until(
            &mut runner,
            &DECLINE_TARGETING_OPPONENT,
            &mut copy_trace,
            |state, _| {
                state.stack.len() == 1
                    && state.stack[0].id == original_id
                    && matches!(state.waiting_for, WaitingFor::Priority { .. })
            },
        );
        assert_eq!(copy_trace.optional_answers.len(), 1);
        assert!(ledger(&runner).is_empty());
        assert_eq!(pool_count(&runner, copy_controller, ManaType::Green), 0);

        let mut original_trace = Trace::default();
        drive_until(
            &mut runner,
            &ACCEPT_TARGETING_OPPONENT,
            &mut original_trace,
            |state, _| {
                state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
            },
        );
        assert_eq!(original_trace.optional_answers.len(), 1);
        assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
        assert_eq!(ledger(&runner), &HashSet::from([(definition, P0)]));
    }
}

/// CR 106.4 + CR 603.4: an accepted copy with X = 0 deposits no mana, so
/// its controller's record remains empty and the original may add three.
#[test]
fn zero_mana_opponent_copy_leaves_original_open() {
    let (mut runner, original_id, copy_id, definition) = carpet_copy_at_retarget(P1, 0, 3);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("P1 retargets its copy to P0");
    let copy = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .unwrap();
    assert_eq!(copy.controller, P1);
    assert_eq!(
        copy.ability()
            .and_then(|ability| ability.trigger_definition_ref.as_ref()),
        Some(&definition)
    );
    let mut copy_trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut copy_trace,
        |state, _| {
            state.stack.len() == 1
                && state.stack[0].id == original_id
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(copy_trace.optional_answers.len(), 1);
    assert!(copy_trace.optional_answers[0].2);
    assert!(ledger(&runner).is_empty());
    assert_eq!(pool_count(&runner, P1, ManaType::Green), 0);

    let mut original_trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut original_trace,
        |state, _| {
            state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
        },
    );
    assert_eq!(original_trace.optional_answers.len(), 1);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(ledger(&runner), &HashSet::from([(definition, P0)]));
}

/// CR 106.4 + CR 605.5a + CR 607.1c: a stack-resolved phase trigger that adds
/// fixed mana records exactly its own definition ref through the prompt-free
/// `Effect::Mana` resolver path.
#[test]
fn stack_resolved_mana_trigger_records_its_own_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Upkeep Bloom", UPKEEP_ADD_GREEN)
        .id();
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        resolved_entry_from(source),
    );

    // Reach: the trigger used the stack and its mana is in the pool, read
    // inside the step before CR 106.4 empties it.
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 1);
    let expected = HashSet::from([(trace.first_ref_from(source), P0)]);
    assert_eq!(ledger(&runner), &expected);
}

/// CR 603.5 + CR 106.4 + CR 607.1c: accepting the optional add and answering
/// the color prompt deposits through the mana-choice continuation, which
/// records the same ref the stack entry carried.
#[test]
fn color_choice_continuation_records_the_resolving_triggers_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Guardless Carpet", MAIN_PHASE_ADD_X)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        resolved_entry_from(source),
    );

    let stack_ref = trace.first_ref_from(source);
    // Reach: the optional prompt was accepted, the color prompt was answered
    // for the resolving ability carrying the stack entry's ref, and X = 3.
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(trace.optional_answers.len(), 1);
    assert_eq!(
        trace.resolving_mana_choice_refs,
        vec![Some(stack_ref.clone())]
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(ledger(&runner), &HashSet::from([(stack_ref, P0)]));
}

/// CR 605.1b + CR 605.4a: a triggered mana ability resolves immediately,
/// without the stack, and still records its own ref.
#[test]
fn stackless_triggered_mana_ability_records_its_own_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flare = scenario
        .add_enchantment_from_oracle(P0, "Mana Flare", MANA_FLARE)
        .id();
    let forest = scenario.add_basic_land(P1, ManaColor::Green);
    let mut runner = scenario.build();
    runner
        .act(GameAction::PassPriority)
        .expect("P1 receives priority to tap the Forest");
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    let selection = grouped
        .get(&forest)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the Forest offers a tap-for-mana action");
    assert!(runner.state().stack.is_empty());

    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tapping the Forest for mana is legal");

    // Reach: no stack entry was created for Mana Flare, and the land's mana
    // plus Mana Flare's bonus mana both reached the pool.
    assert!(runner.state().stack.is_empty());
    assert_eq!(pool_count(&runner, P1, ManaType::Green), 2);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 0);
    let recorded: Vec<&(TriggerDefinitionRef, PlayerId)> = ledger(&runner).iter().collect();
    assert_eq!(recorded.len(), 1, "exactly Mana Flare's occurrence records");
    let flare_incarnation = runner.state().objects[&flare].incarnation;
    assert_eq!(recorded[0].0.source.object_id, flare);
    assert_eq!(recorded[0].0.source.incarnation, flare_incarnation);
    assert_eq!(recorded[0].1, P1);
}

/// CR 605.1a + CR 605.5a: an activated ability with a target is not a mana
/// ability; its `Effect::Mana` resolves from the stack through the same
/// resolver and, carrying no triggered identity, records nothing.
#[test]
fn targeted_activated_mana_effect_records_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prism = scenario
        .add_artifact_from_oracle(P0, "Prism", TARGETED_ACTIVATED_ADD_GREEN)
        .id();
    let mut runner = scenario.build();
    let mut trace = Trace::default();
    runner
        .act(GameAction::ActivateAbility {
            source_id: prism,
            ability_index: 0,
        })
        .expect("the Prism's ability is activatable");

    let target_self = Answers {
        accept_optional: true,
        optional_overrides: Vec::new(),
        target_player: P0,
    };
    drive_until(
        &mut runner,
        &target_self,
        &mut trace,
        resolved_entry_from(prism),
    );

    // Reach: the activation used the stack and the resolver deposited.
    assert_eq!(
        trace.stack_entries,
        vec![(Phase::PreCombatMain, prism, None)]
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 1);
    assert!(ledger(&runner).is_empty());
}

/// CR 106.4: a spell's mana effect resolves through the same resolver and,
/// carrying no triggered identity, records nothing.
#[test]
fn mana_spell_records_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Black);
    let ritual = {
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "Dark Ritual", true, DARK_RITUAL);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        });
        builder.id()
    };
    let mut runner = scenario.build();

    let outcome = runner.cast(ritual).resolve();

    // Reach: the Swamp paid {B} and the spell added {B}{B}{B}.
    let state = outcome.state();
    assert_eq!(
        state.players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Black),
        3
    );
    assert!(state.triggered_abilities_added_mana_this_turn.is_empty());
}

/// CR 113.2c + CR 607.1c: with two mana triggers on one permanent, the one
/// that resolved records exactly its own occurrence, not its sibling's and not
/// a source-wide key.
#[test]
fn sibling_trigger_on_the_same_permanent_records_only_its_own_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(
            P0,
            "Twin Bloom",
            &format!("{UPKEEP_ADD_GREEN}\n{MAIN_PHASE_ADD_X}"),
        )
        .id();
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        resolved_entry_from(source),
    );

    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 1);
    assert_eq!(
        ledger(&runner),
        &HashSet::from([(trace.first_ref_from(source), P0)])
    );
}

/// CR 603.5: declining the optional add puts no mana in a pool, so the
/// trigger records nothing.
#[test]
fn declined_optional_mana_trigger_records_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Guardless Carpet", MAIN_PHASE_ADD_X)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &DECLINE_TARGETING_OPPONENT,
        &mut trace,
        resolved_entry_from(source),
    );

    // Reach: the trigger reached resolution and its optional prompt was answered.
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(trace.saw_entry_from(source));
    assert_eq!(trace.optional_answers.len(), 1);
    assert_eq!(pool_total(&runner, P0), 0);
    assert!(ledger(&runner).is_empty());
}

/// CR 603.5 + CR 106.4: accepting an add whose X is 0 deposits nothing, so
/// the trigger records nothing.
#[test]
fn zero_count_mana_trigger_records_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Guardless Carpet", MAIN_PHASE_ADD_X)
        .id();
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        resolved_entry_from(source),
    );

    // Reach: the trigger reached resolution and the optional add was accepted.
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(trace.saw_entry_from(source));
    assert_eq!(trace.optional_answers.len(), 1);
    assert_eq!(pool_total(&runner, P0), 0);
    assert!(ledger(&runner).is_empty());
}

/// CR 500.1 + CR 603.4: the ledger is a "this turn" record and empties when
/// the next turn begins.
#[test]
fn ledger_clears_at_the_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Upkeep Bloom", UPKEEP_ADD_GREEN)
        .id();
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        resolved_entry_from(source),
    );
    // Reach: the ledger was non-empty before the boundary.
    assert_eq!(
        ledger(&runner),
        &HashSet::from([(trace.first_ref_from(source), P0)])
    );

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        |state, _| state.active_player == P1 && state.phase == Phase::Upkeep,
    );

    assert!(ledger(&runner).is_empty());
}

/// Two clones of `base` whose ledgers hold `left` and `right`, otherwise equal.
fn states_with_ledgers(
    base: &GameState,
    left: &[(TriggerDefinitionRef, PlayerId)],
    right: &[(TriggerDefinitionRef, PlayerId)],
) -> (GameState, GameState) {
    let mut left_state = base.clone();
    let mut right_state = base.clone();
    left_state
        .triggered_abilities_added_mana_this_turn
        .extend(left.iter().cloned());
    right_state
        .triggered_abilities_added_mana_this_turn
        .extend(right.iter().cloned());
    (left_state, right_state)
}

/// Blink `object` (exile, then back) and return the ref of the ETB trigger
/// its new incarnation puts on the stack.
fn blink_and_capture_etb_ref(runner: &mut GameRunner, object: ObjectId) -> TriggerDefinitionRef {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), object, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), object, Zone::Battlefield, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    runner
        .state()
        .stack
        .iter()
        .rev()
        .find(|entry| entry.source_id == object)
        .and_then(|entry| entry.ability())
        .and_then(|ability| ability.trigger_definition_ref.clone())
        .expect("the blinked creature's ETB trigger is on the stack")
}

/// CR 732.2a + CR 104.4b: an unconstrained triggered-mana loop under one
/// incarnation saturates the ledger, so two projected cycles compare equal.
#[test]
fn loop_projection_certifies_a_fixed_source_mana_loop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_from_oracle(P0, "Bloom Priest", 1, 1, ENTERS_ADD_RED)
        .id();
    let mut runner = scenario.build();
    let definition_ref = blink_and_capture_etb_ref(&mut runner, creature);
    let base = runner.state().clone();

    // Control: empty ledgers compare equal.
    let (left, right) = states_with_ledgers(&base, &[], &[]);
    assert!(loop_states_equal_modulo_resources(&left, &right));

    let (left, right) = states_with_ledgers(
        &base,
        &[(definition_ref.clone(), P0)],
        &[(definition_ref, P0)],
    );
    assert!(loop_states_equal_modulo_resources(&left, &right));
}

/// CR 400.7 + CR 732.2a: a loop whose mana-trigger source changes incarnation
/// each cycle mints a fresh ref every cycle; the projection clears the ledger,
/// so the cycles still compare equal while strict equality sees the growth.
#[test]
fn loop_projection_certifies_an_incarnation_changing_mana_loop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_from_oracle(P0, "Bloom Priest", 1, 1, ENTERS_ADD_RED)
        .id();
    let mut runner = scenario.build();
    let first_cycle_ref = blink_and_capture_etb_ref(&mut runner, creature);
    let second_cycle_ref = blink_and_capture_etb_ref(&mut runner, creature);

    // Reach: the refs differ only in the source incarnation.
    assert_ne!(
        first_cycle_ref.source.incarnation,
        second_cycle_ref.source.incarnation
    );
    assert_eq!(
        first_cycle_ref.source.object_id,
        second_cycle_ref.source.object_id
    );
    assert_eq!(first_cycle_ref.occurrence, second_cycle_ref.occurrence);

    let base = runner.state().clone();
    let (left, right) = states_with_ledgers(
        &base,
        &[(first_cycle_ref.clone(), P0)],
        &[(first_cycle_ref, P0), (second_cycle_ref, P0)],
    );
    assert!(loop_states_equal_modulo_resources(&left, &right));
    assert_ne!(
        left, right,
        "strict GameState equality still compares the ledger"
    );
}

/// The ledger serializes deterministically regardless of insertion order and
/// round-trips through deserialization.
#[test]
fn ledger_serializes_deterministically_and_round_trips() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_from_oracle(P0, "Bloom Priest", 1, 1, ENTERS_ADD_RED)
        .id();
    let mut runner = scenario.build();
    let first_ref = blink_and_capture_etb_ref(&mut runner, creature);
    let second_ref = blink_and_capture_etb_ref(&mut runner, creature);
    let base = runner.state().clone();
    let (forward, reverse) = states_with_ledgers(
        &base,
        &[
            (first_ref.clone(), P0),
            (first_ref.clone(), P1),
            (second_ref.clone(), P0),
        ],
        &[
            (second_ref, P0),
            (first_ref.clone(), P1),
            (first_ref.clone(), P0),
        ],
    );

    let forward_json = serde_json::to_string(&forward).expect("GameState serializes");
    let reverse_json = serde_json::to_string(&reverse).expect("GameState serializes");
    assert_eq!(forward_json, reverse_json);

    // Control: the key is present with both entries.
    let value: serde_json::Value = serde_json::from_str(&forward_json).expect("valid JSON");
    let entries = value["triggered_abilities_added_mana_this_turn"]
        .as_array()
        .expect("the ledger serializes as an array");
    assert_eq!(entries.len(), 3);

    let restored: GameState = serde_json::from_str(&forward_json).expect("GameState deserializes");
    assert_eq!(
        restored.triggered_abilities_added_mana_this_turn,
        forward.triggered_abilities_added_mana_this_turn
    );
    let mut without_history = value;
    without_history
        .as_object_mut()
        .expect("GameState is an object")
        .remove("triggered_abilities_added_mana_this_turn");
    let restored_without_history: GameState =
        serde_json::from_value(without_history).expect("missing history defaults");
    assert!(restored_without_history
        .triggered_abilities_added_mana_this_turn
        .is_empty());
    let (other_player, original_player) =
        states_with_ledgers(&base, &[(first_ref.clone(), P1)], &[(first_ref, P0)]);
    assert_ne!(other_player, original_player);
}

/// Braid of Fire on P0's battlefield, driven from the untap step to its
/// cumulative-upkeep payment prompt.
struct BraidAtPrompt {
    runner: GameRunner,
    braid: ObjectId,
    stack_ref: TriggerDefinitionRef,
}

fn braid_at_unless_payment(with_solemnity: bool) -> BraidAtPrompt {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let braid = scenario
        .add_enchantment_from_oracle(P0, "Braid of Fire", BRAID_OF_FIRE)
        .id();
    if with_solemnity {
        scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY);
    }
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        |state, _| matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
    );

    let stack_ref = trace.first_ref_from(braid);
    // Seam guard: the prompt's pending effect carries the stack entry's ref,
    // which is what `costs.rs` reads as `PaymentScope::Resolution`.
    let WaitingFor::UnlessPayment { pending_effect, .. } = &runner.state().waiting_for else {
        unreachable!("the driver stopped at the unless-payment prompt");
    };
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(
        pending_effect.trigger_definition_ref.as_ref(),
        Some(&stack_ref)
    );
    BraidAtPrompt {
        runner,
        braid,
        stack_ref,
    }
}

fn unless_payment_cost(runner: &GameRunner) -> AbilityCost {
    let WaitingFor::UnlessPayment { cost, .. } = &runner.state().waiting_for else {
        panic!("expected the cumulative-upkeep prompt");
    };
    cost.clone()
}

/// CR 702.24a + CR 118.1 + CR 118.12: paying Braid of Fire's cumulative
/// upkeep carries out that triggered ability's own instruction to add {R}, so
/// the deposit records exactly that trigger's ref. CR 106.4: the pool is read
/// inside the upkeep, and the ledger outlives the pool.
#[test]
fn paying_braid_of_fire_cumulative_upkeep_records_its_trigger() {
    let BraidAtPrompt {
        mut runner,
        braid,
        stack_ref,
    } = braid_at_unless_payment(false);
    assert!(matches!(
        unless_payment_cost(&runner),
        AbilityCost::EffectCost { .. }
    ));
    let red_before = pool_count(&runner, P0, ManaType::Red);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("Braid of Fire's mana-adding cost is payable");

    // Reach: still in the upkeep, Braid kept (paid), one age counter, and the
    // payment added {R}.
    let braid_object = &runner.state().objects[&braid];
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(braid_object.zone, Zone::Battlefield);
    assert_eq!(braid_object.counters.get(&CounterType::Age), Some(&1));
    assert_eq!(red_before, 0);
    assert_eq!(pool_count(&runner, P0, ManaType::Red), 1);
    assert_eq!(ledger(&runner), &HashSet::from([(stack_ref.clone(), P0)]));

    let mut trace = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        |state, _| state.phase == Phase::PreCombatMain,
    );

    // CR 106.4: the pool emptied at step end; the per-turn ledger did not.
    assert_eq!(pool_count(&runner, P0, ManaType::Red), 0);
    assert_eq!(ledger(&runner), &HashSet::from([(stack_ref, P0)]));
}

/// CR 702.24a + CR 118.12: declining the cumulative-upkeep cost ("if you
/// don't", sacrifice it) adds no mana, so answering the prompt records nothing.
#[test]
fn declining_braid_of_fire_cumulative_upkeep_records_nothing() {
    let BraidAtPrompt {
        mut runner, braid, ..
    } = braid_at_unless_payment(false);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the cumulative-upkeep cost is legal");

    // Reach: the prompt was answered, Braid was sacrificed, no red was added.
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().objects[&braid].zone, Zone::Graveyard);
    assert_eq!(pool_count(&runner, P0, ManaType::Red), 0);
    assert!(ledger(&runner).is_empty());
}

/// CR 702.24a + CR 118.5 + CR 118.12: with Solemnity preventing the age
/// counter, the cost for zero counters is {0}. Paying it is a real payment
/// that adds no mana, so the ref-bearing trigger records nothing.
#[test]
fn paying_a_zero_counter_cumulative_upkeep_records_nothing() {
    let BraidAtPrompt {
        mut runner, braid, ..
    } = braid_at_unless_payment(true);
    assert_eq!(
        unless_payment_cost(&runner),
        AbilityCost::Mana {
            cost: ManaCost::zero()
        }
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a {0} cumulative-upkeep cost is payable");

    // Reach: the {0} payment was accepted (Braid kept), Solemnity prevented the
    // age counter, and no red was added.
    let braid_object = &runner.state().objects[&braid];
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(braid_object.zone, Zone::Battlefield);
    assert_eq!(braid_object.counters.get(&CounterType::Age), None);
    assert_eq!(pool_count(&runner, P0, ManaType::Red), 0);
    assert!(ledger(&runner).is_empty());
}

/// CONSTRUCTED STATE (not the real trigger pipeline). CR 118.1 + CR 106.4: a
/// resolution-scope mana-adding cost paid for a pending effect that carries no
/// triggered identity deposits mana and records nothing.
#[test]
fn ref_less_resolution_mana_cost_records_nothing() {
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature(P0, "Fixed Mana Cost Source", 1, 1)
        .id();
    let mut runner = scenario.build();
    let pending_effect = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    runner.state_mut().waiting_for = WaitingFor::UnlessPayment {
        player: P0,
        cost: AbilityCost::EffectCost {
            effect: Box::new(Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Blue, ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }),
        },
        pending_effect: Box::new(pending_effect),
        trigger_event: None,
        effect_description: None,
        remaining: vec![],
    };

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("fixed mana effect cost is payable");

    // Reach: the cost arm deposited both units.
    let pool = &runner.state().players[P0.0 as usize].mana_pool.mana;
    assert_eq!(
        pool.iter().map(|unit| unit.color).collect::<Vec<_>>(),
        vec![ManaType::Blue, ManaType::Red]
    );
    assert!(ledger(&runner).is_empty());
}

/// CR 603.4 + CR 607.1c (#9013): Carpet of Flowers added mana in the first main
/// phase, so when the second main phase begins its guard is false and the
/// ability does not trigger at all; no second addition happens this turn.
#[test]
fn carpet_of_flowers_that_added_mana_in_main_one_does_not_trigger_in_main_two() {
    let (mut runner, carpets) = carpet_runner(1, 3);
    let carpet = carpets[0];
    let mut trace = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut trace,
        reached(Phase::End),
    );

    // Reach: in the first main phase Carpet triggered carrying its own
    // identity and added X = 3 (P1 controls three Islands).
    let main_one_ref = trace.only_ref_from_in(carpet, Phase::PreCombatMain);
    assert_eq!(trace.max_green_in(Phase::PreCombatMain), Some(3));

    // CR 603.4: checked when the second main phase begins, the guard is false,
    // so Carpet never reaches the stack there and P0's pool stays empty.
    assert_eq!(trace.refs_from_in(carpet, Phase::PostCombatMain), vec![]);
    let main_two_totals: Vec<usize> = trace
        .pool_readings_in(Phase::PostCombatMain)
        .map(|reading| reading.total)
        .collect();
    assert!(
        !main_two_totals.is_empty(),
        "the driver must have passed through the second main phase"
    );
    assert!(
        main_two_totals.iter().all(|total| *total == 0),
        "no mana may be added in the second main phase, saw {main_two_totals:?}"
    );

    // The guard read the key that the first main phase's deposit recorded.
    assert_eq!(ledger(&runner), &HashSet::from([(main_one_ref, P0)]));
}

/// CR 603.5 + CR 603.4: declining the first-main add puts no mana in a pool,
/// so the guard stays open and the second-main trigger adds X.
#[test]
fn declining_carpet_of_flowers_in_main_one_leaves_main_two_open() {
    let (mut runner, carpets) = carpet_runner(1, 3);
    let carpet = carpets[0];
    let mut main_one = Trace::default();

    drive_until(
        &mut runner,
        &DECLINE_TARGETING_OPPONENT,
        &mut main_one,
        resolved_entry_from_in(carpet, Phase::PreCombatMain),
    );

    // Reach: Carpet triggered in the first main phase, its optional add was
    // declined, and nothing was added or recorded.
    let main_one_ref = main_one.only_ref_from_in(carpet, Phase::PreCombatMain);
    assert_eq!(
        main_one.optional_answers,
        vec![(Phase::PreCombatMain, carpet, false)]
    );
    assert_eq!(pool_total(&runner, P0), 0);
    assert!(ledger(&runner).is_empty());

    let mut main_two = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut main_two,
        resolved_entry_from_in(carpet, Phase::PostCombatMain),
    );

    // CR 113.2c: the same ability, under the same identity, triggers again in
    // the second main phase and adds X = 3.
    assert_eq!(
        main_two.only_ref_from_in(carpet, Phase::PostCombatMain),
        main_one_ref
    );
    assert_eq!(
        main_two.optional_answers,
        vec![(Phase::PostCombatMain, carpet, true)]
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(ledger(&runner), &HashSet::from([(main_one_ref, P0)]));
}

/// CR 603.5 + CR 106.4: accepting the add when X is 0 deposits nothing, so
/// nothing was added with this ability and the second main phase stays open.
#[test]
fn carpet_of_flowers_adding_zero_in_main_one_leaves_main_two_open() {
    let (mut runner, carpets) = carpet_runner(1, 0);
    let carpet = carpets[0];
    let mut main_one = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut main_one,
        resolved_entry_from_in(carpet, Phase::PreCombatMain),
    );

    // Reach: Carpet triggered in the first main phase and the add was
    // accepted; with no Islands X is 0, so no mana arrived and nothing recorded.
    let main_one_ref = main_one.only_ref_from_in(carpet, Phase::PreCombatMain);
    assert_eq!(
        main_one.optional_answers,
        vec![(Phase::PreCombatMain, carpet, true)]
    );
    assert_eq!(pool_total(&runner, P0), 0);
    assert!(ledger(&runner).is_empty());

    let mut main_two = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut main_two,
        resolved_entry_from_in(carpet, Phase::PostCombatMain),
    );

    // CR 603.4: the guard is still true, so the second-main trigger triggers
    // and resolves.
    assert_eq!(
        main_two.only_ref_from_in(carpet, Phase::PostCombatMain),
        main_one_ref
    );
    assert_eq!(
        main_two.optional_answers,
        vec![(Phase::PostCombatMain, carpet, true)]
    );
}

/// CR 113.2c + CR 400.7: two Carpets of Flowers are two abilities with two
/// identities. Mana added by one closes only that one's guard.
#[test]
fn two_carpets_of_flowers_keep_independent_guards() {
    let (mut runner, carpets) = carpet_runner(2, 3);
    let (first, second) = (carpets[0], carpets[1]);
    let accept_first_decline_second = Answers {
        accept_optional: true,
        optional_overrides: vec![(second, false)],
        target_player: P1,
    };
    let mut main_one = Trace::default();

    drive_until(
        &mut runner,
        &accept_first_decline_second,
        &mut main_one,
        |state, trace| {
            trace.saw_entry_from_in(first, Phase::PreCombatMain)
                && resolved_entry_from_in(second, Phase::PreCombatMain)(state, trace)
        },
    );

    // Reach: both Carpets triggered in the first main phase with distinct
    // identities; only the first one's add was accepted, adding X = 3.
    let first_ref = main_one.only_ref_from_in(first, Phase::PreCombatMain);
    let second_ref = main_one.only_ref_from_in(second, Phase::PreCombatMain);
    assert_ne!(first_ref, second_ref);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(ledger(&runner), &HashSet::from([(first_ref.clone(), P0)]));

    let mut main_two = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut main_two,
        resolved_entry_from_in(second, Phase::PostCombatMain),
    );

    // CR 603.4: only the Carpet that added nothing triggers again, and it adds 3.
    assert_eq!(main_two.refs_from_in(first, Phase::PostCombatMain), vec![]);
    assert_eq!(
        main_two.only_ref_from_in(second, Phase::PostCombatMain),
        second_ref
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(
        ledger(&runner),
        &HashSet::from([(first_ref, P0), (second_ref, P0)])
    );
}

/// HOSTILE: CR 607.1c + CR 113.2c: "this ability" is Carpet's own triggered
/// ability, not its permanent. Another mana-producing trigger on the same
/// object (SYNTHETIC `UPKEEP_ADD_GREEN`) records its own identity, and that
/// record must not close Carpet's guard.
#[test]
fn a_sibling_mana_trigger_on_the_same_permanent_leaves_the_carpet_guard_open() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(
            P0,
            "Twin Bloom",
            &format!("{UPKEEP_ADD_GREEN}\n{CARPET_OF_FLOWERS}"),
        )
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    pad_libraries(&mut scenario);
    let mut runner = scenario.build();
    let mut upkeep = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut upkeep,
        resolved_entry_from_in(source, Phase::Upkeep),
    );

    // Reach: the sibling trigger recorded its own identity in the ledger. A
    // pool delta alone would not show that, and a source-keyed guard would
    // now be closed.
    let sibling_ref = upkeep.only_ref_from_in(source, Phase::Upkeep);
    assert_eq!(ledger(&runner), &HashSet::from([(sibling_ref.clone(), P0)]));

    let mut main_one = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut main_one,
        resolved_entry_from_in(source, Phase::PreCombatMain),
    );

    // CR 603.4: Carpet's own guard is open, so it triggers from the same
    // permanent under a different identity and adds X = 3.
    let carpet_ref = main_one.only_ref_from_in(source, Phase::PreCombatMain);
    assert_ne!(carpet_ref, sibling_ref);
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(
        ledger(&runner),
        &HashSet::from([(sibling_ref, P0), (carpet_ref, P0)])
    );
}

/// CR 603.4: "this turn" ends with the turn. The ledger resets when the next
/// turn begins, so Carpet adds mana again in its controller's next first main
/// phase.
#[test]
fn carpet_of_flowers_guard_reopens_on_its_controllers_next_turn() {
    let (mut runner, carpets) = carpet_runner(1, 3);
    let carpet = carpets[0];
    let start_turn = runner.state().turn_number;
    let mut first_turn = Trace::default();

    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut first_turn,
        reached(Phase::End),
    );

    // Reach: this turn Carpet added mana in its first main phase and its
    // guard kept it off the stack in the second.
    let first_turn_ref = first_turn.only_ref_from_in(carpet, Phase::PreCombatMain);
    assert_eq!(first_turn.max_green_in(Phase::PreCombatMain), Some(3));
    assert_eq!(
        first_turn.refs_from_in(carpet, Phase::PostCombatMain),
        vec![]
    );

    let resolved_in_main_one = resolved_entry_from_in(carpet, Phase::PreCombatMain);
    let mut next_turn = Trace::default();
    drive_until(
        &mut runner,
        &ACCEPT_TARGETING_OPPONENT,
        &mut next_turn,
        |state, trace| state.turn_number == start_turn + 2 && resolved_in_main_one(state, trace),
    );

    // Reach: two turns passed and it is P0's turn again.
    assert_eq!(runner.state().turn_number, start_turn + 2);
    assert_eq!(runner.state().active_player, P0);
    // The same ability triggers in this turn's first main phase and adds 3.
    assert_eq!(
        next_turn.only_ref_from_in(carpet, Phase::PreCombatMain),
        first_turn_ref
    );
    assert_eq!(pool_count(&runner, P0, ManaType::Green), 3);
    assert_eq!(ledger(&runner), &HashSet::from([(first_turn_ref, P0)]));
}

/// Every `Effect::Unimplemented` in a definition's resolution chain.
fn unimplemented_effects_in(definition: &AbilityDefinition) -> usize {
    let own = usize::from(matches!(
        definition.effect.as_ref(),
        Effect::Unimplemented { .. }
    ));
    let chained: usize = definition
        .sub_ability
        .as_deref()
        .into_iter()
        .chain(definition.else_ability.as_deref())
        .map(unimplemented_effects_in)
        .sum();
    own + chained
}

/// SHAPE (parser structure, not a runtime row). CR 603.4 + CR 607.1c: Carpet
/// of Flowers' guard parses to the negated self-linked leaf, and the rest of
/// its sentence lowers to the count-sourced mana effect with no gap left.
#[test]
fn carpet_of_flowers_parses_its_guard_and_count_sourced_mana() {
    let parsed = parse_oracle_text(
        CARPET_OF_FLOWERS,
        "Carpet of Flowers",
        &[],
        &["Enchantment".to_string()],
        &[],
    );

    assert_eq!(parsed.triggers.len(), 1);
    let trigger = &parsed.triggers[0];
    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::AddedManaWithThisAbilityThisTurn),
        })
    );
    let execute = trigger
        .execute
        .as_deref()
        .expect("Carpet's trigger must carry its mana effect");
    let Effect::Mana {
        produced: ManaProduction::AnyOneColor { count, .. },
        target,
        ..
    } = execute.effect.as_ref()
    else {
        panic!(
            "Carpet must add mana of any one color, got {:?}",
            execute.effect
        );
    };
    // CR 115.1d: "target opponent" is read by the count, not paid into.
    assert_eq!(
        count,
        &QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Island".to_string())
                        .controller(ControllerRef::TargetOpponent),
                ),
            },
        }
    );
    assert_eq!(
        target,
        &Some(ManaTargetRole::CountSource {
            count_source: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ),
        })
    );
    assert_eq!(unimplemented_effects_in(execute), 0);
    assert!(
        parsed.parse_warnings.is_empty(),
        "Carpet must parse without warnings, got {:?}",
        parsed.parse_warnings
    );
}

/// SHAPE (parser structure, not a runtime row). CR 603.4 + CR 607.1c: Oracle
/// text may spell the guard's contraction with the typographic apostrophe
/// (U+2019), and that spelling must lower to the same negated self-linked leaf.
#[test]
fn carpet_of_flowers_guard_parses_with_a_typographic_apostrophe() {
    let curly_text = CARPET_OF_FLOWERS.replace("haven't", "haven\u{2019}t");
    assert_ne!(
        curly_text, CARPET_OF_FLOWERS,
        "the fixture must carry the contraction"
    );

    let parsed = parse_oracle_text(
        &curly_text,
        "Carpet of Flowers",
        &[],
        &["Enchantment".to_string()],
        &[],
    );

    assert_eq!(parsed.triggers.len(), 1);
    assert_eq!(
        parsed.triggers[0].condition,
        Some(TriggerCondition::Not {
            condition: Box::new(TriggerCondition::AddedManaWithThisAbilityThisTurn),
        })
    );
}

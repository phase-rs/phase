//! CR 117.4 + CR 608.2 + CR 603.3b: the bulk token executor resolves a
//! contiguous run of identical untargeted token triggers in one all-pass
//! boundary. Each member resolves its own captured entry; member 1 takes the
//! full post-action checkpoint, members 2..N−1 keep the production
//! event-trigger collection, and the last member's checkpoint is the caller's
//! ordinary pass. Every row compares the bulk run with the sequential
//! reference (no session, every seat passes, one entry per boundary) on the
//! whole serialized state and the event sequence without `PriorityPassed`.

use engine::ai_support::AiDecisionContract;
use engine::game::engine::{apply, apply_verified_ai_priority_pass};
use engine::game::perf_counters;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityCondition, Comparator, ControllerRef, Effect, FilterProp, ObjectScope, PtValue,
    QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassMode, AutoPassRequest, GameState, StackEntryKind, StackResolutionAutoPassOverlay,
    StackResolutionBudget, StackResolutionEntryFence, StackResolutionPolicy,
    StackResolutionSession, WaitingFor,
};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

const SCUTE_SWARM: &str = "Landfall — Whenever a land you control enters, create a 1/1 green Insect creature token. If you control six or more lands, create a token that's a copy of this creature instead.";
const SPOREMOUND: &str =
    "Landfall — Whenever a land you control enters, create a 1/1 green Saproling creature token.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const DOUBLING_SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const PEREGRIN_TOOK: &str = "If one or more tokens would be created under your control, those tokens plus an additional Food token are created instead.\nSacrifice three Foods: Draw a card.";
const ESIX: &str = "Flying\nThe first time you would create one or more tokens during each of your turns, you may instead choose a creature other than Esix and create that many tokens that are copies of that creature.";
const MIRE_TRITON: &str =
    "Deathtouch\nWhen this creature enters, mill two cards and you gain 2 life.";
const CHAMPION_OF_THE_PARISH: &str =
    "Whenever another Human you control enters, put a +1/+1 counter on this creature.";
const BLESSED_SANCTUARY: &str = "Prevent all noncombat damage that would be dealt to you and creatures you control.\nWhenever a nontoken creature you control enters, create a 2/2 white Unicorn creature token.";
const ELESH_NORN: &str = "Vigilance\nOther creatures you control get +2/+2.\nCreatures your opponents control get -2/-2.";
const ENDANGERED_ARMODON: &str =
    "When you control a creature with toughness 2 or less, sacrifice this creature.";
const ENDREK_SAHR: &str = "Whenever you cast a creature spell, create X 1/1 black Thrull creature tokens, where X is that spell's mana value.\nWhen you control seven or more Thrulls, sacrifice Endrek Sahr.";
const RITE_OF_HARMONY: &str = "Whenever a creature or enchantment you control enters this turn, draw a card.\nFlashback {2}{G}{W}";
const JETMIR: &str = "Creatures you control get +1/+0 and have vigilance as long as you control three or more creatures.\nCreatures you control also get +1/+0 and have trample as long as you control six or more creatures.\nCreatures you control also get +1/+0 and have double strike as long as you control nine or more creatures.";
const INTANGIBLE_VIRTUE: &str = "Creature tokens you control get +1/+1 and have vigilance.";
const BRISTLY_BILL: &str = "Landfall — Whenever a land you control enters, put a +1/+1 counter on target creature.\n{3}{G}{G}: Double the number of +1/+1 counters on each creature you control.";
const LEYLINE_OF_SINGULARITY: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nAll nonland permanents are legendary.";
const MIRROR_GALLERY: &str = "The \"legend rule\" doesn't apply.";

// Synthetic class fixtures (no printed card has the shape). Each carries a
// positive reach guard: the sequential life delta equals the derived figure
// and the parse holds no `Unimplemented`.
const STOP_OBSERVER: &str =
    "Whenever a creature you control enters, if you control ten or more creatures, you gain 1 life.";
const FIXED_POINT_OBSERVER: &str = "Whenever a creature you control enters, if you control two or more creature tokens with power 2 or greater, you gain 1 life.";
const POPULATION_GRANT: &str = "As long as you control eight or more creatures, creatures you control have \"Whenever another creature you control enters, you gain 1 life.\"";
const OPPONENT_CREATURE_OBSERVER: &str =
    "Whenever a creature an opponent controls enters, you gain 1 life.";
const ANOTHER_CREATURE_OBSERVER: &str =
    "Whenever another creature you control enters, you gain 1 life.";

const RUN: usize = 6;

/// Bulk and pipeline-pass counters for one drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counters {
    bulk_entries: u64,
    batched_entries: u64,
    passes: u64,
}

fn counters() -> Counters {
    let bulk = perf_counters::stack_bulk_snapshot();
    Counters {
        bulk_entries: bulk.bulk_entries,
        batched_entries: perf_counters::snapshot().stack_batched_entries,
        passes: bulk.post_action_pipeline_passes,
    }
}

struct Drive {
    state: GameState,
    events: Vec<GameEvent>,
    counters: Counters,
}

/// Two seats in P0's precombat main. `setup` adds the run's sources and the
/// row's permanents; `lands` basic lands are already on P0's battlefield. P0
/// then plays a Forest and answers the order prompt with the identity order.
fn landfall_board(lands: usize, setup: impl FnOnce(&mut GameScenario)) -> GameState {
    landfall_board_keeping(lands, 0, setup)
}

/// `landfall_board`, keeping candidate `keep` (in setup order) when the Forest's
/// checkpoint asks the legend rule (CR 704.5j). Under the identity order the
/// source added at setup index `i` of `n` resolves `n − i`th.
fn landfall_board_keeping(
    lands: usize,
    keep: usize,
    setup: impl FnOnce(&mut GameScenario),
) -> GameState {
    let mut scenario = GameScenario::new_n_player(2, 0x5C07E);
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..lands {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    setup(&mut scenario);
    for _ in 0..30 {
        scenario.add_card_to_library_top(P0, "Forest");
        scenario.add_card_to_library_top(P1, "Forest");
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
    if let WaitingFor::ChooseLegend { candidates, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::ChooseLegend {
                keep: candidates[keep],
            })
            .expect("keeping a candidate is legal");
    }
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
    }
    runner.state().clone()
}

fn scutes(scenario: &mut GameScenario, count: usize) {
    for _ in 0..count {
        scenario
            .add_creature_from_oracle(P0, "Scute Swarm", 1, 1, SCUTE_SWARM)
            .with_subtypes(vec!["Insect"]);
    }
}

fn sporemounds(scenario: &mut GameScenario, count: usize) {
    for _ in 0..count {
        scenario
            .add_creature_from_oracle(P0, "Sporemound", 3, 3, SPOREMOUND)
            .with_subtypes(vec!["Fungus"]);
    }
}

/// Pass priority for whoever holds it until the stack empties or the game
/// asks for something other than a priority pass or a trigger order. An order
/// prompt (CR 603.3b) is answered with the identity order.
fn drive_passes(state: &mut GameState, events: &mut Vec<GameEvent>) {
    for _ in 0..400 {
        let (player, action) = match &state.waiting_for {
            WaitingFor::OrderTriggers { player, triggers } => (
                *player,
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
            ),
            WaitingFor::Priority { player } if !state.stack.is_empty() => {
                (*player, GameAction::PassPriority)
            }
            _ => return,
        };
        events.extend(
            apply(state, player, action)
                .expect("the engine-issued action is legal")
                .events,
        );
    }
    panic!("the stack did not settle within 400 passes");
}

/// The sequential reference: no session; one entry per boundary.
fn sequential(mut state: GameState) -> Drive {
    perf_counters::reset();
    let mut events = Vec::new();
    drive_passes(&mut state, &mut events);
    Drive {
        counters: counters(),
        state,
        events,
    }
}

/// P0 submits Resolve All (a Committed session); the counters cover that
/// submission's dispatch and every later one.
fn resolve_all(mut state: GameState) -> Drive {
    perf_counters::reset();
    let mut events = apply(
        &mut state,
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P0 may request Resolve All")
    .events;
    drive_passes(&mut state, &mut events);
    Drive {
        counters: counters(),
        state,
        events,
    }
}

fn without_priority_passes(events: &[GameEvent]) -> Vec<&GameEvent> {
    events
        .iter()
        .filter(|event| !matches!(event, GameEvent::PriorityPassed { .. }))
        .collect()
}

/// C5.5: the whole serialized state and the event sequence without
/// `PriorityPassed` (priority-window bookkeeping) equal the sequential run.
fn assert_equals_sequential(row: &str, bulk: &Drive, reference: &Drive) {
    assert_eq!(
        without_priority_passes(&bulk.events),
        without_priority_passes(&reference.events),
        "{row}: events differ from the sequential reference"
    );
    let bulk_state = serde_json::to_value(&bulk.state).expect("state serializes");
    let reference_state = serde_json::to_value(&reference.state).expect("state serializes");
    let differing: Vec<&String> = bulk_state
        .as_object()
        .expect("state is an object")
        .iter()
        .filter(|(key, value)| reference_state.get(key.as_str()) != Some(*value))
        .map(|(key, _)| key)
        .collect();
    assert!(
        differing.is_empty(),
        "{row}: serialized state keys differ from the sequential reference: {differing:?}"
    );
}

fn tokens_named(state: &GameState, name: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].is_token && state.objects[id].name == name)
        .count()
}

/// Run both drives from one S0 and assert C5.5 equality.
fn parity(row: &str, s0: GameState) -> (Drive, Drive) {
    let reference = sequential(s0.clone());
    let bulk = resolve_all(s0);
    assert_equals_sequential(row, &bulk, &reference);
    (bulk, reference)
}

/// Reach guard for a synthetic Oracle fixture: its parse holds no
/// `Effect::Unimplemented`.
fn assert_parsed(state: &GameState, name: &str) {
    let object = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .find(|object| object.name == name)
        .expect("the synthetic fixture is on the battlefield");
    let parsed = serde_json::to_string(&(&object.trigger_definitions, &object.static_definitions))
        .expect("definitions serialize");
    assert!(
        !parsed.contains("Unimplemented"),
        "{name} must parse without Unimplemented: {parsed}"
    );
    assert!(
        !object.trigger_definitions.is_empty() || !object.static_definitions.is_empty(),
        "{name} must parse to at least one ability"
    );
}

fn edit_stacked_abilities(state: &mut GameState, edit: impl Fn(&mut ResolvedAbility)) {
    for entry in state.stack.iter_mut() {
        if let StackEntryKind::TriggeredAbility { ability, .. } = &mut entry.kind {
            edit(ability);
        }
    }
}

fn edit_stacked_token_specs(state: &mut GameState, edit: impl Fn(&mut Effect)) {
    edit_stacked_abilities(state, |ability| edit(&mut ability.effect));
}

/// A1: N Insect-branch Scute triggers (one land < 6, CR 608.2c) resolve as
/// one bulk boundary, equal to sequential resolution.
#[test]
fn insect_run_resolves_in_one_bulk_boundary() {
    for n in [RUN, 40] {
        let s0 = landfall_board(0, |s| scutes(s, n));
        assert_eq!(s0.stack.len(), n, "reach guard: {n} Scute triggers");
        let (bulk, reference) = parity("A1", s0);
        assert_eq!(tokens_named(&reference.state, "Insect"), n);
        assert_eq!(bulk.counters.bulk_entries, n as u64);
        assert_eq!(bulk.counters.batched_entries, n as u64);
    }
}

/// A11: inside one dispatch that closes a verified four-seat cohort over the
/// whole run, the pipeline passes do not grow with N: member 1's checkpoint
/// plus the caller's pass (CR 117.4 + CR 704.3).
#[test]
fn bulk_boundary_pipeline_passes_do_not_grow_with_the_run() {
    for n in [RUN, 40] {
        let mut state = four_seat_board(n);
        for player in [P0, P1, PlayerId(2)] {
            verified_pass(&mut state, player);
        }
        assert_eq!(state.stack.len(), n, "reach guard: nothing resolved yet");
        perf_counters::reset();
        verified_pass(&mut state, PlayerId(3));
        assert!(state.stack.is_empty());
        assert_eq!(tokens_named(&state, "Insect"), n);
        assert_eq!(counters().bulk_entries, n as u64);
        assert_eq!(
            counters().passes,
            2,
            "N = {n}: member 1's checkpoint and the caller's pass"
        );
    }
}

/// Four seats, P0 active with `n` Scute Swarms and no land; P0 plays a Forest
/// and the identity order puts `n` Insect-branch triggers on the stack.
fn four_seat_board(n: usize) -> GameState {
    let mut scenario = GameScenario::new_n_player(4, 0x5C07E);
    scenario.at_phase(Phase::PreCombatMain);
    scutes(&mut scenario, n);
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
    }
    runner.state().clone()
}

fn verified_pass(state: &mut GameState, player: PlayerId) {
    let contract = AiDecisionContract::issue(state, player);
    apply_verified_ai_priority_pass(state, player, &contract, GameAction::PassPriority)
        .expect("a verified AI priority pass is legal");
}

/// A4: a mandatory token-doubling replacement applies once per member's
/// creation event (CR 614.1a), so each member creates two Insects.
#[test]
fn token_doubling_replacements_stay_per_member() {
    for (name, oracle) in [
        ("Doubling Season", DOUBLING_SEASON),
        ("Parallel Lives", PARALLEL_LIVES),
    ] {
        let s0 = landfall_board(0, |s| {
            scutes(s, RUN);
            s.add_enchantment_from_oracle(P0, name, oracle);
        });
        let (bulk, reference) = parity(name, s0);
        assert_eq!(tokens_named(&reference.state, "Insect"), 2 * RUN);
        assert_eq!(bulk.counters.bulk_entries, RUN as u64);
    }
}

/// A5: Peregrin Took adds one Food per creation event (CR 614.1a); each
/// member resolves through the production token handler.
#[test]
fn per_creation_event_replacement_adds_one_food_per_member() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P0, "Peregrin Took", 2, 3, PEREGRIN_TOOK);
    });
    let (bulk, reference) = parity("A5", s0);
    assert_eq!(tokens_named(&reference.state, "Insect"), RUN);
    assert_eq!(tokens_named(&reference.state, "Food"), RUN);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

/// A6: Esix's optional replacement (CR 616.1) refuses at admission; the first
/// member pauses with the sequential prompt.
#[test]
fn optional_replacement_refuses_and_pauses_as_sequential() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P0, "Esix, Fractal Bloom", 4, 4, ESIX);
    });
    let (bulk, reference) = parity("A6", s0);
    let WaitingFor::ReplacementChoice {
        candidate_count, ..
    } = reference.state.waiting_for
    else {
        panic!("the first member pauses for Esix's optional replacement");
    };
    // The engine shows an optional replacement as two choices: apply, decline.
    assert_eq!(candidate_count, 2);
    assert_eq!(reference.state.stack.len(), RUN - 1);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A7: Soul Warden (P1) observes each Insect (CR 603.6a); member 1's
/// checkpoint collects its trigger, so the bulk path refuses.
#[test]
fn matching_observer_refuses_at_member_one() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
            .with_subtypes(vec!["Human", "Cleric"]);
    });
    let (bulk, reference) = parity("A7", s0);
    assert_eq!(reference.state.players[1].life, 20 + RUN as i32);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A7b: observers that never apply to the produced token (a self-referential
/// ETB, an opponent's "another Human you control", a nontoken filter) do not
/// refuse; paired with A7.
#[test]
fn non_matching_observers_are_admitted() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P0, "Mire Triton", 2, 1, MIRE_TRITON)
            .with_subtypes(vec!["Zombie", "Merfolk"]);
        s.add_creature_from_oracle(P1, "Champion of the Parish", 1, 1, CHAMPION_OF_THE_PARISH)
            .with_subtypes(vec!["Human", "Soldier"]);
        s.add_enchantment_from_oracle(P0, "Blessed Sanctuary", BLESSED_SANCTUARY);
    });
    let (bulk, reference) = parity("A7b", s0);
    assert_eq!(tokens_named(&reference.state, "Insect"), RUN);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

/// A8: Elesh Norn's −2/−2 kills each Insect as an SBA (CR 704.5f) at the
/// checkpoint after it enters, so member 1's checkpoint is not inert.
#[test]
fn per_checkpoint_sba_death_refuses() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P1, "Elesh Norn, Grand Cenobite", 4, 7, ELESH_NORN);
    });
    let (bulk, reference) = parity("A8", s0);
    assert_eq!(tokens_named(&reference.state, "Insect"), 0);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-L (labelled class fixture: the stacked token specs are edited to be
/// legendary): the second legendary Insect trips the legend rule (CR 704.5j),
/// a pairwise SBA member 1's checkpoint cannot show, so admission refuses.
#[test]
fn legendary_token_run_refuses_at_the_pairwise_sba_gate() {
    let mut s0 = landfall_board(0, |s| scutes(s, RUN));
    edit_stacked_token_specs(&mut s0, |effect| {
        if let Effect::Token { supertypes, .. } = effect {
            supertypes.push(Supertype::Legendary);
        }
    });
    let (bulk, reference) = parity("A8-L", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 2");
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(reference.state.stack.len(), RUN - 2);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-X (labelled class fixture: the stacked token specs are edited to an X/X
/// Saproling where X is the source's power): three distinct-source Sporemounds
/// of power 2, 0 and 3 create different tokens. Member 2's 0/0 Saproling dies
/// at its own checkpoint (CR 704.5f) before member 3 resolves, which member
/// 1's checkpoint cannot show, so the run is not bulk-admitted.
#[test]
fn distinct_source_token_specs_refuse_the_run() {
    let mut s0 = landfall_board(0, |s| {
        // Under the identity order setup index 1 resolves second.
        for power in [2, 0, 3] {
            s.add_creature_from_oracle(P0, "Sporemound", power, 3, SPOREMOUND)
                .with_subtypes(vec!["Fungus"]);
        }
    });
    assert_eq!(s0.stack.len(), 3, "reach guard: three Sporemound triggers");
    edit_stacked_token_specs(&mut s0, |effect| {
        if let Effect::Token {
            power, toughness, ..
        } = effect
        {
            let source_power = PtValue::Quantity(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            });
            *power = source_power.clone();
            *toughness = source_power;
        }
    });
    let (bulk, reference) = parity("A8-X", s0);
    assert_eq!(
        tokens_named(&reference.state, "Saproling"),
        2,
        "reach guard: member 2's 0/0 Saproling dies"
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// Three distinct Sporemound sources; the first to resolve is an artifact
/// creature and the board's only artifact, so "another artifact you control"
/// counts 0 for member 1 and 1 for members 2 and 3.
fn artifact_led_sporemound_board() -> GameState {
    let s0 = landfall_board(0, |s| {
        // Under the identity order setup index 2 resolves first.
        for artifact in [false, false, true] {
            let mut sporemound = s.add_creature_from_oracle(P0, "Sporemound", 3, 3, SPOREMOUND);
            sporemound.with_subtypes(vec!["Fungus"]);
            if artifact {
                sporemound.as_artifact_creature();
            }
        }
    });
    assert_eq!(s0.stack.len(), 3, "reach guard: three Sporemound triggers");
    s0
}

/// The number of other artifacts the member's controller controls: the count
/// excludes the member's own source (`FilterProp::Another`).
fn other_artifacts_you_control() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact)
                    .controller(ControllerRef::You)
                    .properties(vec![
                        FilterProp::Another,
                        FilterProp::InZone {
                            zone: Zone::Battlefield,
                        },
                    ]),
            ),
        },
    }
}

/// "If you control another artifact".
fn you_control_another_artifact() -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: other_artifacts_you_control(),
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    }
}

/// Edit a token effect to a 0/0 Germ.
fn germ(effect: &mut Effect) {
    if let Effect::Token {
        name,
        power,
        toughness,
        ..
    } = effect
    {
        *name = "Germ".to_string();
        *power = PtValue::Fixed(0);
        *toughness = PtValue::Fixed(0);
    }
}

fn germs_created(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::TokenCreated { name, .. } if name == "Germ"))
        .count()
}

/// A8-CI (labelled class fixture: each stacked trigger gets the sub "if you
/// control another artifact, create a 0/0 Germ instead"): member 1's artifact
/// source makes its swap unmet while members 2 and 3 meet it (CR 608.2c). Their
/// Germs die at their own checkpoints (CR 704.5f), which member 1's checkpoint
/// cannot show, so the run is not bulk-admitted.
#[test]
fn distinct_source_instead_verdicts_refuse_the_run() {
    let mut s0 = artifact_led_sporemound_board();
    edit_stacked_abilities(&mut s0, |ability| {
        let mut swapped = ability.effect.clone();
        germ(&mut swapped);
        ability.sub_ability = Some(Box::new(
            ResolvedAbility::new(swapped, vec![], ability.source_id, ability.controller).condition(
                AbilityCondition::ConditionInstead {
                    inner: Box::new(you_control_another_artifact()),
                },
            ),
        ));
    });
    let (bulk, reference) = parity("A8-CI", s0);
    assert_eq!(tokens_named(&reference.state, "Saproling"), 1);
    assert_eq!(
        (
            germs_created(&reference.events),
            tokens_named(&reference.state, "Germ")
        ),
        (2, 0),
        "reach guard: members 2 and 3 create 0/0 Germs that die"
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-RC (labelled class fixture: each stacked trigger's root effect becomes a
/// 0/0 Germ gated on "if you control another artifact"): member 1 creates
/// nothing while members 2 and 3 create Germs that die at their own
/// checkpoints (CR 608.2c, CR 704.5f), so the run is not bulk-admitted.
#[test]
fn root_condition_refuses_the_run() {
    let mut s0 = artifact_led_sporemound_board();
    edit_stacked_abilities(&mut s0, |ability| {
        germ(&mut ability.effect);
        ability.condition = Some(you_control_another_artifact());
    });
    let (bulk, reference) = parity("A8-RC", s0);
    assert_eq!(
        (
            germs_created(&reference.events),
            tokens_named(&reference.state, "Germ")
        ),
        (2, 0),
        "reach guard: members 2 and 3 create 0/0 Germs that die"
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-RR (labelled class fixture: each stacked trigger's root effect becomes a
/// 0/0 Germ repeated once for each other artifact you control): member 1
/// repeats zero times while members 2 and 3 each create a Germ that dies at
/// its own checkpoint (CR 608.2c, CR 704.5f), so the run is not bulk-admitted.
#[test]
fn root_repeat_for_refuses_the_run() {
    let mut s0 = artifact_led_sporemound_board();
    edit_stacked_abilities(&mut s0, |ability| {
        germ(&mut ability.effect);
        ability.repeat_for = Some(other_artifacts_you_control());
    });
    let (bulk, reference) = parity("A8-RR", s0);
    assert_eq!(
        (
            germs_created(&reference.events),
            tokens_named(&reference.state, "Germ")
        ),
        (2, 0),
        "reach guard: members 2 and 3 create 0/0 Germs that die"
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// Six Scute Swarm sources under distinct names (labelled class fixture: each
/// carries Scute Swarm's verbatim Oracle text, so Leyline of Singularity's
/// legendary sources do not share a name).
fn distinctly_named_scutes(scenario: &mut GameScenario) {
    for index in 1..=RUN {
        scenario
            .add_creature_from_oracle(P0, &format!("Scute Swarm {index}"), 1, 1, SCUTE_SWARM)
            .with_subtypes(vec!["Insect"]);
    }
}

/// A8-LC: Leyline of Singularity makes each Insect legendary in layer 4
/// (CR 613.1d) before its member's checkpoint, though the printed token spec
/// is not legendary. The second Insect trips the legend rule (CR 704.5j) at
/// member 2's checkpoint, so admission must read the layered supertypes.
#[test]
fn layered_legendary_token_run_refuses_at_the_pairwise_sba_gate() {
    // Reach guard: without Leyline the distinctly named sources form one
    // admitted run, so the refusal below comes from Leyline.
    let control = landfall_board(0, distinctly_named_scutes);
    let (control_bulk, _) = parity("A8-LC control", control);
    assert_eq!(control_bulk.counters.bulk_entries, RUN as u64);

    let s0 = landfall_board(0, |s| {
        distinctly_named_scutes(s);
        s.add_enchantment_from_oracle(P0, "Leyline of Singularity", LEYLINE_OF_SINGULARITY);
    });
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    assert!(
        s0.battlefield.iter().all(|id| {
            let object = &s0.objects[id];
            object.card_types.supertypes.contains(&Supertype::Legendary)
                != object.card_types.core_types.contains(&CoreType::Land)
        }),
        "reach guard: Leyline makes every nonland permanent legendary"
    );
    for entry in &s0.stack {
        let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
            panic!("only Scute triggers are on the stack");
        };
        let Effect::Token { supertypes, .. } = &ability.effect else {
            panic!("each Scute trigger creates a token");
        };
        assert!(
            supertypes.is_empty(),
            "reach guard: the printed token spec is not legendary"
        );
    }
    let (bulk, reference) = parity("A8-LC", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 2");
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(reference.state.stack.len(), RUN - 2);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8b: Endangered Armodon's state trigger (CR 603.8) is crossed by member 1
/// (a 1/1 Insect): member 1's checkpoint refuses.
#[test]
fn state_trigger_crossed_at_member_one_refuses() {
    let s0 = landfall_board(0, |s| {
        for _ in 0..RUN {
            s.add_creature_from_oracle(P0, "Scute Swarm", 3, 3, SCUTE_SWARM)
                .with_subtypes(vec!["Insect"]);
        }
        s.add_creature_from_oracle(P0, "Endangered Armodon", 4, 5, ENDANGERED_ARMODON);
    });
    let (bulk, reference) = parity("A8b", s0);
    assert!(
        !reference
            .state
            .battlefield
            .iter()
            .any(|id| reference.state.objects[id].name == "Endangered Armodon"),
        "the Armodon is sacrificed"
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8b-S (labelled class fixture: the run's tokens are edited to Thrulls):
/// Endrek Sahr's state trigger (CR 603.8) is crossed strictly mid-run, at
/// member 2 (5 Thrulls + 2). The state-trigger gate refuses the run.
#[test]
fn state_trigger_crossed_mid_run_refuses() {
    let mut s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P0, "Endrek Sahr, Master Breeder", 2, 2, ENDREK_SAHR)
            .with_subtypes(vec!["Human", "Wizard"]);
        for _ in 0..5 {
            s.add_creature_from_oracle(P0, "Thrull", 1, 1, "")
                .with_subtypes(vec!["Thrull"]);
        }
    });
    edit_stacked_token_specs(&mut s0, |effect| {
        if let Effect::Token { name, types, .. } = effect {
            *name = "Thrull".to_string();
            for t in types.iter_mut().filter(|t| *t == "Insect") {
                *t = "Thrull".to_string();
            }
        }
    });
    let (bulk, reference) = parity("A8b-S", s0);
    assert_eq!(tokens_named(&reference.state, "Thrull"), RUN);
    assert!(
        !reference
            .state
            .battlefield
            .iter()
            .any(|id| reference.state.objects[id].name == "Endrek Sahr, Master Breeder"),
        "Endrek Sahr is sacrificed"
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8b-D: Rite of Harmony's delayed trigger (CR 603.7) matches each Insect,
/// member 1's included ("a creature ... you control enters this turn"), and
/// goes on the stack above the remaining Scute entries (CR 603.3b); each
/// firing draws P0 a card.
#[test]
fn delayed_trigger_matched_by_the_run_refuses() {
    let mut scenario = GameScenario::new_n_player(2, 0x5C07E);
    scenario.at_phase(Phase::PreCombatMain);
    scutes(&mut scenario, RUN);
    for _ in 0..30 {
        scenario.add_card_to_library_top(P0, "Forest");
        scenario.add_card_to_library_top(P1, "Forest");
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rite of Harmony", true, RITE_OF_HARMONY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: Rite of Harmony installed its delayed trigger"
    );
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
    }
    let s0 = runner.state().clone();
    assert_eq!(s0.stack.len(), RUN);
    let hand_at_s0 = s0.players[0].hand.len();
    let library_at_s0 = s0.players[0].library.len();
    let (bulk, reference) = parity("A8b-D", s0);
    assert_eq!(tokens_named(&reference.state, "Insect"), RUN);
    // Reach guard: the delayed trigger fired once per member, each put on
    // the stack above the remaining Scute entries.
    let delayed_firings = reference
        .events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
        .count();
    assert_eq!(delayed_firings, RUN);
    // "draw a card" per firing: P0's hand +RUN and library -RUN.
    assert_eq!(reference.state.players[0].hand.len(), hand_at_s0 + RUN);
    assert_eq!(
        reference.state.players[0].library.len(),
        library_at_s0 - RUN
    );
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A9: a completed targeted landfall trigger (Bristly Bill, ordered to the
/// bottom, CR 603.3b) does not stop the Scute run above it from bulk
/// resolution; ordered to the top, it resolves alone first (CR 608.2b).
#[test]
fn run_beside_a_completed_targeted_trigger() {
    for bristly_on_top in [false, true] {
        let mut scenario = GameScenario::new_n_player(2, 0x5C07E);
        scenario.at_phase(Phase::PreCombatMain);
        let bristly = scenario
            .add_creature_from_oracle(P0, "Bristly Bill, Spine Sower", 2, 2, BRISTLY_BILL)
            .with_subtypes(vec!["Plant", "Druid"])
            .as_legendary()
            .id();
        scutes(&mut scenario, RUN);
        let forest = scenario.add_land_to_hand(P0, "Forest").id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&forest].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: forest,
                card_id,
            })
            .expect("playing the Forest is legal");
        let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
            panic!("seven landfall triggers raise the order prompt");
        };
        let bristly_index = triggers
            .iter()
            .position(|trigger| trigger.source_id == bristly)
            .expect("Bristly Bill's trigger is in the prompt");
        let others = (0..triggers.len()).filter(|&index| index != bristly_index);
        let order = if bristly_on_top {
            others.chain(std::iter::once(bristly_index)).collect()
        } else {
            std::iter::once(bristly_index).chain(others).collect()
        };
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("the engine-issued order is legal");
        let WaitingFor::TriggerTargetSelection { target_slots, .. } =
            runner.state().waiting_for.clone()
        else {
            panic!("Bristly Bill's trigger pauses for its target");
        };
        runner
            .act(GameAction::ChooseTarget {
                target: Some(target_slots[0].legal_targets[0].clone()),
            })
            .expect("the engine-issued target is legal");
        let s0 = runner.state().clone();
        assert_eq!(s0.stack.len(), RUN + 1);
        assert!(s0.pending_trigger_event_batch.is_empty());
        let (bulk, reference) = parity("A9", s0);
        assert_eq!(tokens_named(&reference.state, "Insect"), RUN);
        assert_eq!(bulk.counters.bulk_entries, RUN as u64);
    }
}

/// A10: the bulk path consumes at most the session-authorized limit
/// (CR 117.3b + CR 117.4). Four seats: an ordinary pass by P1 authorizes one
/// entry; a fence of 3 or a budget of 3 authorizes three.
#[test]
fn bulk_run_stays_inside_the_authorized_limit() {
    let seats = BTreeSet::from([P0, P1, PlayerId(2), PlayerId(3)]);

    // Recheck with P1 neither verified nor standing-passed: one entry.
    let mut state = four_seat_board(RUN);
    verified_pass(&mut state, P0);
    apply(&mut state, P1, GameAction::PassPriority).expect("P1 may pass");
    verified_pass(&mut state, PlayerId(2));
    perf_counters::reset();
    verified_pass(&mut state, PlayerId(3));
    assert_eq!(state.stack.len(), RUN - 1);
    assert_eq!(counters().bulk_entries, 0);

    // A fence of three, then a budget of three over the whole run.
    for (fenced, budget) in [
        (3, StackResolutionBudget::Unlimited),
        (
            RUN,
            StackResolutionBudget::Limited(NonZeroU32::new(3).unwrap()),
        ),
    ] {
        let mut state = four_seat_board(RUN);
        let entries = state
            .stack
            .iter()
            .rev()
            .take(fenced)
            .map(StackResolutionEntryFence::capture)
            .collect();
        for &player in &seats {
            state.auto_pass.insert(
                player,
                AutoPassMode::UntilStackEmpty {
                    initial_stack_len: state.stack.len(),
                    policy: StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
                },
            );
        }
        state.stack_resolution_session = Some(StackResolutionSession {
            entries,
            cursor: 0,
            representatives: seats.clone(),
            verified_pass_representatives: BTreeSet::from([P0, P1, PlayerId(2)]),
            budget,
            policy: StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
            auto_pass_overlay: StackResolutionAutoPassOverlay {
                baseline: BTreeMap::new(),
            },
        });
        state.priority_passes = BTreeSet::from([P0, P1, PlayerId(2)]);
        state.priority_player = PlayerId(3);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(3),
        };
        perf_counters::reset();
        verified_pass(&mut state, PlayerId(3));
        assert_eq!(state.stack.len(), RUN - 3);
        assert_eq!(counters().bulk_entries, 3);
    }
}

/// A2 (P5a-S1 retired into it): with six lands each Scute trigger takes the
/// copy branch (CR 608.2c) and copies its own source (CR 707.2). The sources
/// share copiable values, so the run resolves in one bulk boundary, equal to
/// sequential resolution.
#[test]
fn met_copy_instead_run_resolves_in_one_bulk_boundary() {
    for n in [RUN, 40] {
        let s0 = landfall_board(5, |s| scutes(s, n));
        assert_eq!(s0.stack.len(), n, "reach guard: {n} Scute triggers");
        let (bulk, reference) = parity("A2", s0);
        assert_eq!(tokens_named(&reference.state, "Scute Swarm"), n);
        assert_eq!(bulk.counters.bulk_entries, n as u64);
        assert_eq!(bulk.counters.batched_entries, n as u64);
    }
}

/// L-FIX-C: a copy-branch token reaches its member's checkpoint unlayered,
/// and Intangible Virtue makes it 2/2 there (CR 613.4c). The intervening-if
/// (CR 603.4) is false for member 1 (one token) and true for members 2..6:
/// P0 gains 5. Member 1's token changes at its checkpoint, so the run refuses.
#[test]
fn copy_branch_token_changed_by_its_checkpoint_refuses() {
    let s0 = landfall_board(5, |s| {
        scutes(s, RUN);
        s.add_enchantment_from_oracle(P0, "Intangible Virtue", INTANGIBLE_VIRTUE);
        s.add_creature_from_oracle(P0, "Fixed Point Observer", 0, 4, FIXED_POINT_OBSERVER);
    });
    assert_parsed(&s0, "Fixed Point Observer");
    let (bulk, reference) = parity("L-FIX-C", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(reference.state.players[0].life, 20 + 5);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// Seven Scute Swarm sources; `divergent` (an index in setup order) is a 2/2
/// Insect Mutant (labelled class fixture: a source whose copiable values,
/// CR 707.2, differ from the others', as a modified copy's would).
fn scutes_with_divergent(scenario: &mut GameScenario, divergent: usize) {
    for index in 0..=RUN {
        let (power, subtypes) = if index == divergent {
            (2, vec!["Insect", "Mutant"])
        } else {
            (1, vec!["Insect"])
        };
        scenario
            .add_creature_from_oracle(P0, "Scute Swarm", power, power, SCUTE_SWARM)
            .with_subtypes(subtypes);
    }
}

fn mutant_tokens(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|object| {
            object.is_token && object.card_types.subtypes.iter().any(|s| s == "Mutant")
        })
        .count()
}

/// A3: one source's copiable values diverge from the rest (CR 707.2), at the
/// bottom, the middle or the top of the run. The copy arm admits only a run
/// whose every member shares the top member's values, so the run is refused
/// and each member copies its own source: exactly one Mutant token.
/// Preservation row; A2 is its discriminating sibling.
#[test]
fn divergent_copy_source_refuses_the_run() {
    for divergent in [0, RUN / 2, RUN] {
        let s0 = landfall_board(5, |s| scutes_with_divergent(s, divergent));
        assert_eq!(
            s0.stack.len(),
            RUN + 1,
            "reach guard: {} Scute triggers",
            RUN + 1
        );
        let (bulk, reference) = parity("A3", s0);
        assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN + 1);
        assert_eq!(mutant_tokens(&reference.state), 1);
        assert_eq!(
            bulk.counters.bulk_entries, 0,
            "divergent source at {divergent}"
        );
    }
}

/// A3-mid-L (labelled class fixture: a legendary source carrying Scute
/// Swarm's verbatim text, placed mid-run). Its member's copy shares the
/// source's name, so the legend rule (CR 704.5j) asks at that member's
/// checkpoint, which only a whole-run copiable-value check keeps from being
/// elided. Preservation row; A2 is its discriminating sibling.
#[test]
fn legendary_source_mid_run_refuses_the_run() {
    let s0 = landfall_board(5, |s| {
        scutes(s, 3);
        s.add_creature_from_oracle(P0, "Scute Legend", 1, 1, SCUTE_SWARM)
            .with_subtypes(vec!["Insect"])
            .as_legendary();
        scutes(s, 3);
    });
    assert_eq!(
        s0.stack.len(),
        RUN + 1,
        "reach guard: {} Scute triggers",
        RUN + 1
    );
    let (bulk, reference) = parity("A3-mid-L", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after the legendary source's member");
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(tokens_named(&reference.state, "Scute Legend"), 1);
    // The legendary source, added at setup index 3 of 7, resolves fourth.
    assert_eq!(reference.state.stack.len(), RUN - 3);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A5-C: on the copy branch Peregrin Took still adds one Food per creation
/// event (CR 614.1a): each member creates its own copy, so six copies and six
/// Foods.
#[test]
fn copy_branch_replacement_adds_one_food_per_member() {
    let s0 = landfall_board(5, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P0, "Peregrin Took", 2, 3, PEREGRIN_TOOK);
    });
    let (bulk, reference) = parity("A5-C", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(tokens_named(&reference.state, "Food"), RUN);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

/// Every battlefield Scute Swarm moves to the graveyard through the production
/// zone-move primitive, which records last-known information (CR 400.7,
/// CR 608.2h) exactly as an instant-speed sweeper cast in response would.
fn remove_scute_sources(state: &mut GameState) {
    let sources: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].name == "Scute Swarm")
        .collect();
    let mut events = Vec::new();
    for id in sources {
        move_to_zone(state, id, Zone::Graveyard, &mut events);
    }
}

/// L5 (P5b-1) and A2-D (P5b-2): every source left the battlefield after its
/// met landfall trigger. Each member copies its own source's last-known
/// copiable values (CR 608.2h + CR 707.2; Scute Swarm's ruling), so the run
/// makes six Scute Swarm tokens. The copy arm reads those values through the
/// copy handler's own authority, and they are shared, so the run is admitted
/// as one bulk boundary.
#[test]
fn departed_self_copy_run_still_creates_every_copy() {
    let mut s0 = landfall_board(5, |s| scutes(s, RUN));
    remove_scute_sources(&mut s0);
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    assert!(
        !s0.battlefield
            .iter()
            .any(|id| s0.objects[id].name == "Scute Swarm"),
        "reach guard: no source is on the battlefield"
    );
    let (bulk, reference) = parity("L5", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

/// Six Scute Swarms under Leyline of Singularity (every nonland permanent is
/// legendary): the Forest's checkpoint asks the legend rule (CR 704.5j) before
/// the triggers are put on the stack (CR 117.5), and P0 keeps the source added
/// at `keep`. The other five sources are in the graveyard.
fn leyline_board(keep: usize) -> GameState {
    let s0 = landfall_board_keeping(5, keep, |s| {
        scutes(s, RUN);
        s.add_enchantment_from_oracle(P0, "Leyline of Singularity", LEYLINE_OF_SINGULARITY);
    });
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    let kept = s0
        .battlefield
        .iter()
        .filter(|id| s0.objects[id].name == "Scute Swarm")
        .count();
    assert_eq!(kept, 1, "reach guard: the legend rule kept one source");
    s0
}

/// The resolution position (1 = top) of the trigger whose source is on the
/// battlefield.
fn kept_source_position(state: &GameState) -> usize {
    state
        .stack
        .iter()
        .rev()
        .position(|entry| state.battlefield.contains(&entry.source_id))
        .expect("the kept source has a trigger on the stack")
        + 1
}

/// A8-C: the kept source's trigger resolves first. Its copy is legendary in
/// layer 4 (CR 613.1d) and shares the source's name, so the legend rule
/// (CR 704.5j) asks after member 1 between the source and its copy.
#[test]
fn legendary_self_copy_asks_after_member_one() {
    let s0 = leyline_board(RUN - 1);
    assert_eq!(
        kept_source_position(&s0),
        1,
        "reach guard: kept source on top"
    );
    let (bulk, reference) = parity("A8-C", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 1");
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(reference.state.stack.len(), RUN - 1);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// L6 (P5b-1; A8-C-GY's derived reading): the kept source's trigger resolves
/// fourth, so members 1–3 copy sources the legend rule put into the
/// graveyard. Each copies its source's last-known copiable values
/// (CR 608.2h), so member 1's copy is a legendary Scute Swarm under Leyline
/// and the legend rule (CR 704.5j) asks after member 1, over the kept source
/// and that copy, with five entries still on the stack. A8-C-GY (P5b-2): the
/// copy arm admits the run on the departed members' last-known values, and
/// member 1's checkpoint asks the legend rule, so the bulk path refuses it.
#[test]
fn self_copy_run_with_departed_sources_asks_after_member_one() {
    let s0 = leyline_board(2);
    assert_eq!(
        kept_source_position(&s0),
        RUN - 2,
        "reach guard: kept source mid-run"
    );
    let kept_source = s0
        .battlefield
        .iter()
        .copied()
        .find(|id| s0.objects[id].name == "Scute Swarm")
        .expect("reach guard: one source on the battlefield");
    let (bulk, reference) = parity("L6", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 1");
    };
    assert_eq!(candidates.len(), 2);
    assert!(candidates.contains(&kept_source));
    assert!(candidates
        .iter()
        .any(|id| reference.state.objects[id].is_token
            && reference.state.objects[id].name == "Scute Swarm"));
    assert_eq!(reference.state.stack.len(), RUN - 1);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-C, departed copies legendary through a layer-4 effect: under Leyline
/// every source has left the battlefield, the kept one too. Member 1's copy is
/// the only legendary Scute Swarm; member 2's is the second, so the legend
/// rule (CR 704.5j) asks after member 2. Member 1's copy reaches its
/// checkpoint unlayered and Leyline makes it legendary there (CR 613.1d), so
/// the member-1 fixed point refuses the run, with the layered
/// pairwise-supertype check on that copy behind it.
#[test]
fn departed_legendary_self_copies_ask_after_member_two() {
    let mut s0 = leyline_board(RUN - 1);
    remove_scute_sources(&mut s0);
    assert!(
        !s0.battlefield
            .iter()
            .any(|id| s0.objects[id].name == "Scute Swarm"),
        "reach guard: no source is on the battlefield"
    );
    let (bulk, reference) = parity("A8-C departed, layered", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 2");
    };
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .all(|id| reference.state.objects[id].is_token));
    assert_eq!(reference.state.stack.len(), RUN - 2);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-C, departed copies legendary from their copiable values (labelled class
/// fixture: six legendary sources carrying Scute Swarm's verbatim text under
/// one name, as a "legendary in addition" copy exception would make them,
/// CR 707.9b). The Forest's checkpoint keeps one (CR 704.5j), then every
/// source leaves. Each copy is legendary from its source's last-known
/// copiable values (CR 608.2h + CR 707.2), so member 1's copy is unchanged by
/// its checkpoint and alone; member 2's is the second, so the legend rule asks
/// after member 2. Only the layered pairwise-supertype check on member 1's
/// copy refuses the run.
#[test]
fn departed_printed_legendary_self_copies_ask_after_member_two() {
    let mut s0 = landfall_board(5, |s| {
        for _ in 0..RUN {
            s.add_creature_from_oracle(P0, "Scute Swarm", 1, 1, SCUTE_SWARM)
                .with_subtypes(vec!["Insect"])
                .as_legendary();
        }
    });
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    remove_scute_sources(&mut s0);
    let (bulk, reference) = parity("A8-C departed, copiable", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 2");
    };
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .all(|id| reference.state.objects[id].is_token));
    assert_eq!(reference.state.stack.len(), RUN - 2);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-C-MG (labelled class fixture: six legendary Scute Swarms, as their
/// copiable values would be under a "legendary in addition" copy exception,
/// CR 707.9b). Mirror Gallery switches the legend rule off, so no member asks;
/// each copy is legendary before its checkpoint, and the pairwise supertype
/// check refuses the run conservatively.
#[test]
fn legendary_self_copy_without_the_legend_rule_refuses() {
    let s0 = landfall_board(5, |s| {
        for _ in 0..RUN {
            s.add_creature_from_oracle(P0, "Scute Swarm", 1, 1, SCUTE_SWARM)
                .with_subtypes(vec!["Insect"])
                .as_legendary();
        }
        s.add_artifact_from_oracle(P0, "Mirror Gallery", MIRROR_GALLERY);
    });
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    let (bulk, reference) = parity("A8-C-MG", s0);
    assert!(matches!(
        reference.state.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// A8-L2 (labelled class fixture: six legendary sources under distinct names,
/// each carrying Scute Swarm's verbatim text). Each member copies its own
/// legendary source (CR 707.2), so the legend rule (CR 704.5j) asks after
/// member 1 between the source and its copy.
#[test]
fn legendary_distinct_sources_ask_after_member_one() {
    let s0 = landfall_board(5, |s| {
        for index in 1..=RUN {
            s.add_creature_from_oracle(P0, &format!("Scute Legend {index}"), 1, 1, SCUTE_SWARM)
                .with_subtypes(vec!["Insect"])
                .as_legendary();
        }
    });
    let (bulk, reference) = parity("A8-L2", s0);
    let WaitingFor::ChooseLegend { candidates, .. } = &reference.state.waiting_for else {
        panic!("the legend rule must ask after member 1");
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(reference.state.stack.len(), RUN - 1);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// L-SHAPE-C: on the copy branch a copy token reaches its member's
/// checkpoint unlayered, so Jetmir's statics (CR 611.3a) change member 1's
/// copy there (+1/+0 and vigilance with three or more creatures), and the
/// member-1 fixed point refuses the run before the layer verdict is read.
#[test]
fn copy_branch_population_conditioned_static_refuses() {
    let s0 = landfall_board(5, |s| {
        scutes(s, 2);
        s.add_creature_from_oracle(P0, "Jetmir, Nexus of Revels", 5, 4, JETMIR);
    });
    assert_eq!(s0.stack.len(), 2, "reach guard: 2 Scute triggers");
    let (bulk, reference) = parity("L-SHAPE-C", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), 2);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// L-GRANT-C: the copy-branch sibling of L-GRANT. The grant crosses at member
/// 2 (six Scute Swarms and two copies make eight creatures); from member k >= 2
/// on, each of the 5 + k other creatures triggers (CR 603.10):
/// 7 + 8 + 9 + 10 + 11 = 45. Member 1's copy is unchanged at its checkpoint,
/// so the layer verdict (CR 611.3a) is the gate that refuses.
#[test]
fn copy_branch_population_granted_trigger_refuses() {
    let s0 = landfall_board(5, |s| {
        scutes(s, RUN);
        s.add_enchantment_from_oracle(P0, "Population Grant", POPULATION_GRANT);
    });
    assert_parsed(&s0, "Population Grant");
    let (bulk, reference) = parity("L-GRANT-C", s0);
    assert_eq!(reference.state.players[0].life, 20 + 45);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// Labelled class fixture: `count` sources carrying Scute Swarm's verbatim
/// text plus `line`, so every copy carries `line` too (CR 707.2).
fn scutes_with_line(scenario: &mut GameScenario, count: usize, line: &str) {
    let text = format!("{SCUTE_SWARM}\n{line}");
    for _ in 0..count {
        scenario
            .add_creature_from_oracle(P0, "Scute Swarm", 1, 1, &text)
            .with_subtypes(vec!["Insect"]);
    }
}

/// Reach guard for `scutes_with_line`: a source parsed the landfall trigger
/// and the appended trigger line.
fn assert_carries_two_triggers(state: &GameState) {
    assert_parsed(state, "Scute Swarm");
    let source = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .find(|object| object.name == "Scute Swarm")
        .expect("a source is on the battlefield");
    assert_eq!(
        source.trigger_definitions.len(),
        2,
        "reach guard: landfall plus the appended trigger"
    );
}

/// A7b-C (copy-arm sibling of A7b): every copy carries an observer of
/// creatures an opponent controls. P0's copies never match its trigger
/// condition (CR 603.2), so P0 gains no life and the copy arm admits the run
/// whole: an observer is decided by applicability, never by its event type
/// alone (D5.4).
#[test]
fn copy_branch_non_matching_observer_is_admitted() {
    let s0 = landfall_board(5, |s| scutes_with_line(s, RUN, OPPONENT_CREATURE_OBSERVER));
    assert_carries_two_triggers(&s0);
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    let (bulk, reference) = parity("A7b-C", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(reference.state.players[0].life, 20);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

/// U-STOP-C (copy-arm sibling of U-STOP): every copy carries "whenever
/// another creature you control enters". Every source has left, so member 1's
/// copy enters alone and its checkpoint is inert. Member 1's copy sees member
/// 2's (CR 603.2), so the per-member collection ends the first boundary at
/// member 2 (CR 603.3b). Member k's copy triggers each of the k − 1 earlier
/// copies: 0 + 1 + 2 + 3 + 4 + 5 = 15.
#[test]
fn copy_branch_run_ends_at_the_first_member_whose_collection_is_non_empty() {
    let mut s0 = landfall_board(5, |s| scutes_with_line(s, RUN, ANOTHER_CREATURE_OBSERVER));
    assert_carries_two_triggers(&s0);
    remove_scute_sources(&mut s0);
    assert_eq!(s0.stack.len(), RUN, "reach guard: {RUN} Scute triggers");
    assert!(
        !s0.battlefield
            .iter()
            .any(|id| s0.objects[id].name == "Scute Swarm"),
        "reach guard: no source is on the battlefield"
    );
    let (bulk, reference) = parity("U-STOP-C", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(reference.state.players[0].life, 20 + 15);
    assert_eq!(bulk.counters.bulk_entries, 2);
}

/// L-SHAPE: Jetmir's population-conditioned statics (CR 611.3a) are perturbed
/// by each entry, so the layer verdict refuses the run.
#[test]
fn population_conditioned_static_refuses() {
    let s0 = landfall_board(0, |s| {
        scutes(s, 2);
        s.add_creature_from_oracle(P0, "Jetmir, Nexus of Revels", 5, 4, JETMIR);
    });
    let (bulk, _) = parity("L-SHAPE", s0);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// L-GRANT: a granted trigger that exists only once the board reaches eight
/// creatures (crossed at member 2 of a plain token trigger run, whose tokens
/// are first layered at the checkpoint). From member k ≥ 2 on, each of the
/// 5 + k other creatures triggers (CR 603.10): 7 + 8 + 9 + 10 + 11 = 45.
#[test]
fn population_granted_trigger_refuses() {
    let s0 = landfall_board(0, |s| {
        sporemounds(s, RUN);
        s.add_enchantment_from_oracle(P0, "Population Grant", POPULATION_GRANT);
    });
    assert_parsed(&s0, "Population Grant");
    let (bulk, reference) = parity("L-GRANT", s0);
    assert_eq!(reference.state.players[0].life, 20 + 45);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// L-FIX: Intangible Virtue makes each Saproling 2/2 at its first layer pass
/// (CR 613.4c). The intervening-if (CR 603.4) is false for member 1 (one
/// token) and true for members 2..6: P0 gains 5. Member 1's token changes at
/// its checkpoint, so the run refuses.
#[test]
fn effect_applying_to_the_entrant_refuses() {
    let s0 = landfall_board(0, |s| {
        sporemounds(s, RUN);
        s.add_enchantment_from_oracle(P0, "Intangible Virtue", INTANGIBLE_VIRTUE);
        s.add_creature_from_oracle(P0, "Fixed Point Observer", 0, 4, FIXED_POINT_OBSERVER);
    });
    assert_parsed(&s0, "Fixed Point Observer");
    let (bulk, reference) = parity("L-FIX", s0);
    assert_eq!(reference.state.players[0].life, 20 + 5);
    assert_eq!(bulk.counters.bulk_entries, 0);
}

/// U-STOP (D5.4 stop rule): an observer whose intervening-if (CR 603.4) first
/// holds at member 3 (seven creatures + 3 = ten). Its trigger goes on the
/// stack above member 4 (CR 603.3b), so the first boundary is members 1..3;
/// members 3..6 each gain 1 life.
#[test]
fn run_ends_at_the_first_member_whose_collection_is_non_empty() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_creature_from_oracle(P0, "Stop Observer", 0, 4, STOP_OBSERVER);
    });
    assert_parsed(&s0, "Stop Observer");
    let (bulk, reference) = parity("U-STOP", s0);
    assert_eq!(reference.state.players[0].life, 20 + 4);
    assert_eq!(bulk.counters.bulk_entries, 3);
}

/// C5a.7 sibling of L-FIX: Scute's Insect branch has a sub-ability, so the
/// chain flushes layers inside the member's resolution and its Insect is
/// already 2/2 under Intangible Virtue when the checkpoint runs; the run is
/// admitted. A plain token trigger (Sporemound, L-FIX) reaches the
/// checkpoint unlayered.
#[test]
fn insect_branch_token_is_layered_before_its_checkpoint() {
    let s0 = landfall_board(0, |s| {
        scutes(s, RUN);
        s.add_enchantment_from_oracle(P0, "Intangible Virtue", INTANGIBLE_VIRTUE);
    });
    let (bulk, reference) = parity("C5a.7", s0);
    assert_eq!(tokens_named(&reference.state, "Insect"), RUN);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

/// Reach guard for L-FIX and L-GRANT: the plain token trigger run alone
/// (six Sporemounds, no sub-ability) is admitted, so their refusals come from
/// the permanent each row adds.
#[test]
fn plain_token_trigger_run_is_admitted() {
    let s0 = landfall_board(0, |s| sporemounds(s, RUN));
    let (bulk, reference) = parity("plain", s0);
    assert_eq!(tokens_named(&reference.state, "Saproling"), RUN);
    assert_eq!(bulk.counters.bulk_entries, RUN as u64);
}

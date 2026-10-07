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
use engine::types::ability::Effect;
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

// Synthetic class fixtures (no printed card has the shape). Each carries a
// positive reach guard: the sequential life delta equals the derived figure
// and the parse holds no `Unimplemented`.
const STOP_OBSERVER: &str =
    "Whenever a creature you control enters, if you control ten or more creatures, you gain 1 life.";
const FIXED_POINT_OBSERVER: &str = "Whenever a creature you control enters, if you control two or more creature tokens with power 2 or greater, you gain 1 life.";
const POPULATION_GRANT: &str = "As long as you control eight or more creatures, creatures you control have \"Whenever another creature you control enters, you gain 1 life.\"";

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

fn edit_stacked_token_specs(state: &mut GameState, edit: impl Fn(&mut Effect)) {
    for entry in state.stack.iter_mut() {
        if let StackEntryKind::TriggeredAbility { ability, .. } = &mut entry.kind {
            edit(&mut ability.effect);
        }
    }
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

/// S1 (preservation): a met copy-instead run (six lands, CR 608.2c + CR 707.2)
/// is refused by the production verdict and the sequential proof consumes it
/// in one boundary, as before the bulk path existed.
#[test]
fn met_copy_instead_run_stays_on_the_proof() {
    let s0 = landfall_board(5, |s| scutes(s, RUN));
    let (bulk, reference) = parity("S1", s0);
    assert_eq!(tokens_named(&reference.state, "Scute Swarm"), RUN);
    assert_eq!(bulk.counters.bulk_entries, 0);
    assert_eq!(bulk.counters.batched_entries, RUN as u64);
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

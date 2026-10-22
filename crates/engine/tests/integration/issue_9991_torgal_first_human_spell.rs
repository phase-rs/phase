//! Torgal's first qualifying cast installs a spell-specific entry replacement.
//! Oracle text verified in MTGJSON AtomicCards.json.
//! CR 603.2: only the matching event triggers the ability.
//! CR 109.5 + CR 603.3a: "you" remains the trigger's controller.
//! CR 113.7a: source departure does not remove its pending ability.
//! CR 611.2d + CR 608.2h: the population is determined at trigger resolution.
//! CR 614.12: counters modify entry, before enters triggers observe the creature.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId};

const TORGAL: &str = "Whenever you cast your first Human creature spell each turn, that creature enters with an additional +1/+1 counter on it for each Dog and/or Wolf you control.\n{T}: Add one mana of any color.";
const SERVANT: &str = "This creature enters with a +1/+1 counter on it.\nWhen this creature dies, put X +1/+1 counters on target creature you control, where X is the number of +1/+1 counters on this creature.";
const OUTCASTER: &str = "When this creature enters, if you control a creature with power 4 or greater, draw a card.\nPlot {1}{G} (You may pay {1}{G} and exile this card from your hand. Cast it as a sorcery on a later turn without paying its mana cost. Plot only as a sorcery.)";

fn torgal(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Torgal, A Fine Hound", 2, 2, TORGAL)
        .with_subtypes(vec!["Wolf"])
        .id()
}

fn animal(scenario: &mut GameScenario, player: PlayerId, name: &str, types: Vec<&str>) -> ObjectId {
    scenario
        .add_creature(player, name, 2, 2)
        .with_subtypes(types)
        .id()
}

fn human(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 1, 1, "")
        .with_subtypes(vec!["Human"])
        .with_mana_cost(ManaCost::zero())
        .flash()
        .id()
}

fn setup_union() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = torgal(&mut scenario);
    animal(&mut scenario, P0, "Dog", vec!["Dog"]);
    animal(&mut scenario, P0, "Wolf", vec!["Wolf"]);
    animal(&mut scenario, P0, "Dog Wolf", vec!["Dog", "Wolf"]);
    animal(&mut scenario, P0, "Unrelated", vec!["Elf"]);
    animal(&mut scenario, P1, "Opponent Dog", vec!["Dog"]);
    animal(&mut scenario, P1, "Opponent Wolf", vec!["Wolf"]);
    (scenario, source)
}

fn counters(state: &GameState, spell: ObjectId, expected: u32) {
    let object = &state.objects[&spell];
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "reach guard: entrant resolved"
    );
    // CR 614.12: counters are on the entering permanent, not the installer.
    assert_eq!(
        object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        expected,
        "{}",
        object.name
    );
}

fn complete(state: &GameState, spell: ObjectId, expected: u32) {
    counters(state, spell, expected);
    assert!(state.stack.is_empty(), "entire stack must finish");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        state.waiting_for
    );
}

fn commit(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().stack.len(), 2, "creature and Torgal trigger");
}

fn install(runner: &mut GameRunner) {
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1, "creature remains on stack");
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "rider installed"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn a_union_counts_source_once_and_excludes_opponents() {
    let (mut scenario, _) = setup_union();
    let spell = human(&mut scenario, "First Human");
    let mut runner = scenario.build();
    let result = runner.cast(spell).resolve();
    complete(result.state(), spell, 4);
}

#[test]
fn b_non_humans_do_not_consume_first_human_and_c_second_human_gets_none() {
    let (mut scenario, _) = setup_union();
    let nonhuman = scenario
        .add_creature_to_hand(P0, "Non-Human before", 1, 1)
        .with_subtypes(vec!["Elf"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let first = human(&mut scenario, "First Human");
    let second = human(&mut scenario, "Second Human");
    let later_nonhuman = scenario
        .add_creature_to_hand(P0, "Non-Human after", 1, 1)
        .with_subtypes(vec!["Elf"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(nonhuman).resolve();
    complete(runner.state(), nonhuman, 0);
    runner.cast(first).resolve();
    complete(runner.state(), first, 4);
    runner.cast(later_nonhuman).resolve();
    complete(runner.state(), later_nonhuman, 0);
    runner.cast(second).resolve();
    complete(runner.state(), second, 0);
    counters(runner.state(), first, 4); // positive guard for every negative above
}

#[test]
fn d_next_turn_first_human_triggers_again() {
    let (mut scenario, _) = setup_union();
    scenario.with_library_top(P1, &["Draw-step card"]);
    let first = human(&mut scenario, "First turn Human");
    let next = human(&mut scenario, "Next turn Human");
    // Fixture Humans have flash so P0 can cast on P1's turn legally.
    let mut runner = scenario.build();
    runner.cast(first).resolve();
    complete(runner.state(), first, 4);
    for _ in 0..80 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).unwrap();
            }
            waiting => panic!("unexpected turn progression prompt: {waiting:?}"),
        }
    }
    assert_eq!(
        runner.state().active_player,
        P1,
        "production turn progression"
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    runner.cast(next).resolve();
    complete(runner.state(), next, 4);
}

#[derive(Clone, Copy)]
enum Timing {
    BeforeResolution,
    AfterResolution,
}
#[derive(Clone, Copy)]
enum PopulationChange {
    RemoveDog,
    RemoveSource,
    TransferSource,
    Grow,
}

fn population_response(change: PopulationChange, timing: Timing) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = torgal(&mut scenario);
    let dog = animal(&mut scenario, P0, "Dog", vec!["Dog"]);
    animal(&mut scenario, P0, "Wolf", vec!["Wolf"]);
    for name in [
        "Opponent Dog A",
        "Opponent Dog B",
        "Opponent Dog C",
        "Opponent Dog D",
    ] {
        animal(&mut scenario, P1, name, vec!["Dog"]);
    }
    let spell = human(&mut scenario, "Timing Human");
    let (response, actor) = match change {
        PopulationChange::RemoveDog | PopulationChange::RemoveSource => {
            let victim = match change {
                PopulationChange::RemoveDog => dog,
                PopulationChange::RemoveSource => source,
                PopulationChange::TransferSource | PopulationChange::Grow => unreachable!(),
            };
            (
                scenario
                    .add_spell_to_hand(P0, "Synthetic removal response", true)
                    .with_mana_cost(ManaCost::zero())
                    .with_ability(Effect::Destroy {
                        target: TargetFilter::SpecificObject { id: victim },
                        cant_regenerate: false,
                    })
                    .id(),
                P0,
            )
        }
        PopulationChange::TransferSource => (
            scenario
                .add_spell_to_hand(P1, "Synthetic control response", true)
                .with_mana_cost(ManaCost::zero())
                .with_ability(Effect::GainControl {
                    target: TargetFilter::SpecificObject { id: source },
                })
                .id(),
            P1,
        ),
        PopulationChange::Grow => (
            scenario
                .add_creature_to_hand(P0, "Flash Dog", 2, 2)
                .with_subtypes(vec!["Dog"])
                .with_mana_cost(ManaCost::zero())
                .flash()
                .id(),
            P0,
        ),
    };
    let mut runner = scenario.build();
    commit(&mut runner, spell);
    if let Timing::AfterResolution = timing {
        install(&mut runner);
    }
    if !matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == actor) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    runner.cast(response).commit();
    runner.resolve_top();
    match change {
        PopulationChange::RemoveDog => {
            assert_eq!(runner.state().objects[&dog].zone, Zone::Graveyard)
        }
        PopulationChange::RemoveSource => {
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard)
        }
        PopulationChange::TransferSource => {
            assert_eq!(runner.state().objects[&source].controller, P1)
        }
        PopulationChange::Grow => {
            assert_eq!(runner.state().objects[&response].zone, Zone::Battlefield)
        }
    }
    runner.advance_until_stack_empty();
    let expected = match (timing, change) {
        (Timing::AfterResolution, _) => 3,
        (Timing::BeforeResolution, PopulationChange::Grow) => 4,
        (
            Timing::BeforeResolution,
            PopulationChange::RemoveDog
            | PopulationChange::RemoveSource
            | PopulationChange::TransferSource,
        ) => 2,
    };
    complete(runner.state(), spell, expected);
}

#[test]
fn e1_dog_removed_before_resolution_is_not_counted() {
    population_response(PopulationChange::RemoveDog, Timing::BeforeResolution);
}
#[test]
fn e2_dog_removed_after_resolution_remains_counted() {
    population_response(PopulationChange::RemoveDog, Timing::AfterResolution);
}
#[test]
fn f1_source_removed_before_resolution_ability_survives() {
    population_response(PopulationChange::RemoveSource, Timing::BeforeResolution);
}
#[test]
fn f2_source_removed_after_resolution_remains_counted() {
    population_response(PopulationChange::RemoveSource, Timing::AfterResolution);
}
#[test]
fn g1_control_change_before_resolution_keeps_original_you() {
    population_response(PopulationChange::TransferSource, Timing::BeforeResolution);
}
#[test]
fn g2_control_change_after_resolution_does_not_recount() {
    population_response(PopulationChange::TransferSource, Timing::AfterResolution);
}
#[test]
fn growth_before_resolution_is_counted() {
    population_response(PopulationChange::Grow, Timing::BeforeResolution);
}
#[test]
fn growth_after_resolution_is_not_counted() {
    population_response(PopulationChange::Grow, Timing::AfterResolution);
}

#[test]
fn source_alone_counts_one_and_departure_timing_distinguishes_zero() {
    // All three branches share a positive control: the same lone Wolf gives one.
    for departure in [
        None,
        Some(Timing::BeforeResolution),
        Some(Timing::AfterResolution),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = torgal(&mut scenario);
        let spell = human(&mut scenario, "Lone-source Human");
        let removal = scenario
            .add_spell_to_hand(P0, "Synthetic source removal", true)
            .with_mana_cost(ManaCost::zero())
            .with_ability(Effect::Destroy {
                target: TargetFilter::SpecificObject { id: source },
                cant_regenerate: false,
            })
            .id();
        let mut runner = scenario.build();
        commit(&mut runner, spell);
        if let Some(Timing::AfterResolution) = departure {
            install(&mut runner);
        }
        if departure.is_some() {
            runner.cast(removal).commit();
            runner.resolve_top();
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        }
        runner.advance_until_stack_empty();
        let expected = match departure {
            Some(Timing::BeforeResolution) => 0,
            None | Some(Timing::AfterResolution) => 1,
        };
        complete(runner.state(), spell, expected);
    }
}

#[test]
fn h_self_entry_replacement_and_torgal_each_apply_once() {
    let (mut scenario, source) = setup_union();
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Servant of the Scale", 0, 0, SERVANT)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    // CR 616.1: the affected creature's controller orders both replacements.
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        candidates,
        kind,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "both entry replacements must be offered: {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    assert_eq!(*candidate_count, 2);
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == spell));
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == source)
        .expect("Torgal rider is a candidate");
    runner.act(GameAction::ChooseReplacement { index }).unwrap();
    runner.advance_until_stack_empty();
    complete(runner.state(), spell, 5);
}

#[test]
fn i_casting_torgal_itself_gives_no_counter_with_human_positive_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, "Torgal, A Fine Hound", 2, 2, TORGAL)
        .with_subtypes(vec!["Wolf"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let spell = human(&mut scenario, "Later Human");
    let mut runner = scenario.build();
    runner.cast(source).resolve();
    complete(runner.state(), source, 0);
    runner.cast(spell).resolve();
    complete(runner.state(), spell, 1);
    counters(runner.state(), source, 0);
}

#[test]
fn j_flash_second_human_cannot_consume_first_spells_installed_rider() {
    let (mut scenario, _) = setup_union();
    let first = human(&mut scenario, "Original Human");
    let second = human(&mut scenario, "Interloping Human");
    let mut runner = scenario.build();
    commit(&mut runner, first);
    install(&mut runner);
    runner.cast(second).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "second Human must not trigger Torgal"
    );
    runner.resolve_top();
    counters(runner.state(), second, 0);
    assert_eq!(runner.state().objects[&first].zone, Zone::Stack);
    runner.advance_until_stack_empty();
    complete(runner.state(), first, 4);
    counters(runner.state(), second, 0);
}

#[test]
fn k_entry_counters_enable_beastbond_outcaster_etb_draw() {
    let (mut scenario, _) = setup_union();
    let draw = scenario.add_card_to_library_top(P0, "Entry witness draw");
    // No resident creature has power 4. Outcaster's own entry counters alone
    // make its intervening-if true as its real ETB trigger is checked.
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Beastbond Outcaster", 3, 3, OUTCASTER)
        .with_subtypes(vec!["Human", "Druid"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    complete(runner.state(), spell, 4);
    assert_eq!(
        runner.state().objects[&draw].zone,
        Zone::Hand,
        "ETB observed the counter-enhanced power"
    );
}

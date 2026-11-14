//! Mass counter removal (`Effect::RemoveCounter { scope: EffectScope::All }`,
//! phase-rs/phase#9899): "remove all counters from all permanents" (Aether
//! Snap), "remove all fate counters from all permanents" (Oblivion Stone),
//! "remove all gold counters from all creatures" (Aurification), "remove two
//! loyalty counters from each planeswalker" (Pestilent Haze) and "remove a
//! -1/-1 counter from each creature you control" (Heartmender) name no target
//! (CR 115.10a). Each card is built from its verbatim Oracle text and driven
//! through the production cast / activation / trigger pipeline. Before the
//! change each of these asked for ONE target and removed counters from it alone.
//!
//! Temporal Distortion's bare-plural population ("permanents that player
//! controls") is covered in `controller_anaphor_binds_through_context.rs`.

use engine::game::game_object::{PhaseOutCause, PhaseStatus};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;

const AETHER_SNAP: &str = "Remove all counters from all permanents and exile all tokens.";
const OBLIVION_STONE: &str = "{4}, {T}: Put a fate counter on target permanent.\n\
{5}, {T}, Sacrifice this artifact: Destroy each nonland permanent without a fate counter on it, then remove all fate counters from all permanents.";
const AURIFICATION: &str = "Whenever a creature deals damage to you, put a gold counter on it.\n\
Each creature with a gold counter on it is a Wall in addition to its other creature types and has defender. (Those creatures can't attack.)\n\
When this enchantment leaves the battlefield, remove all gold counters from all creatures.";
const PESTILENT_HAZE: &str = "Choose one \u{2014}\n\
\u{2022} All creatures get -2/-2 until end of turn.\n\
\u{2022} Remove two loyalty counters from each planeswalker.";
const HEARTMENDER: &str = "At the beginning of your upkeep, remove a -1/-1 counter from each creature you control.\n\
Persist (When this creature dies, if it had no -1/-1 counters on it, return it to the battlefield under its owner's control with a -1/-1 counter on it.)";

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

fn ice() -> CounterType {
    CounterType::Generic("ice".to_string())
}

fn fate() -> CounterType {
    CounterType::Generic("fate".to_string())
}

fn gold() -> CounterType {
    CounterType::Generic("gold".to_string())
}

fn count_of(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

/// `(object, kind, count)` of every `CounterRemoved` event, in emission order.
fn removals(events: &[GameEvent]) -> Vec<(ObjectId, CounterType, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CounterRemoved {
                object_id,
                counter_type,
                count,
            } => Some((*object_id, counter_type.clone(), *count)),
            _ => None,
        })
        .collect()
}

/// R1 + R6: Aether Snap removes every counter of every kind from every phased-in
/// permanent of both players, with no target prompt, and only then exiles the
/// tokens (CR 608.2c). A phased-out permanent is treated as though it does not
/// exist (CR 702.26b) and keeps its counter. Per object, the kinds are removed in
/// `CounterType` order, so the event sequence does not depend on `HashMap`
/// iteration order.
#[test]
fn aether_snap_removes_every_counter_from_every_phased_in_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Four counter kinds on one object (determinism row). Not +1/+1 together
    // with -1/-1: CR 704.5q would annihilate them pairwise before the cast.
    let multi = scenario.add_creature(P0, "Multi Bear", 3, 3).id();
    scenario.with_counter(multi, CounterType::Plus1Plus1, 2);
    scenario.with_counter(multi, ice(), 1);
    scenario.with_counter(multi, charge(), 3);
    scenario.with_counter(multi, CounterType::Time, 1);
    // A land with a charge counter.
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_counter(land, charge(), 2);
    // The opponent's creature with a -1/-1 counter.
    let theirs = scenario.add_creature(P1, "Their Bear", 3, 3).id();
    scenario.with_counter(theirs, CounterType::Minus1Minus1, 1);
    // A token with a +1/+1 counter: its counters go first, then it is exiled.
    let token = scenario.add_creature(P1, "Their Token", 1, 1).id();
    scenario.with_counter(token, CounterType::Plus1Plus1, 1);
    // A phased-out creature (CR 702.26b) and its phased-in twin (reach guard).
    let phased_out = scenario.add_creature(P1, "Phased Bear", 2, 2).id();
    scenario.with_counter(phased_out, CounterType::Plus1Plus1, 1);
    let phased_in = scenario.add_creature(P1, "Present Bear", 2, 2).id();
    scenario.with_counter(phased_in, CounterType::Plus1Plus1, 1);
    let snap = scenario
        .add_spell_to_hand_from_oracle(P0, "Aether Snap", false, AETHER_SNAP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&token).unwrap().is_token = true;
        state.objects.get_mut(&phased_out).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
    }

    // No target is declared: a target prompt would panic the cast driver.
    let outcome = runner.cast(snap).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves without a prompt: {:?}",
        outcome.final_waiting_for()
    );

    for id in [multi, land, theirs, phased_in] {
        assert!(
            runner.state().objects[&id].counters.is_empty(),
            "every counter is removed from {id:?}: {:?}",
            runner.state().objects[&id].counters
        );
    }
    assert_eq!(
        count_of(&runner, phased_out, &CounterType::Plus1Plus1),
        1,
        "a phased-out permanent keeps its counter (CR 702.26b)"
    );
    assert_ne!(
        runner.state().objects.get(&token).map(|t| t.zone),
        Some(Zone::Battlefield),
        "tokens are exiled after the removal"
    );

    let mut removed = removals(outcome.events());
    // Per object, kinds come off in ascending `CounterType` order.
    let multi_kinds: Vec<CounterType> = removed
        .iter()
        .filter(|(id, _, _)| *id == multi)
        .map(|(_, kind, _)| kind.clone())
        .collect();
    assert_eq!(multi_kinds.len(), 4, "{removed:?}");
    assert!(
        multi_kinds.windows(2).all(|pair| pair[0] < pair[1]),
        "kinds are removed in CounterType order: {multi_kinds:?}"
    );
    // The multiset of removals is exactly what was on the phased-in permanents.
    removed.sort();
    let mut expected = vec![
        (multi, CounterType::Plus1Plus1, 2),
        (multi, ice(), 1),
        (multi, charge(), 3),
        (multi, CounterType::Time, 1),
        (land, charge(), 2),
        (theirs, CounterType::Minus1Minus1, 1),
        (token, CounterType::Plus1Plus1, 1),
        (phased_in, CounterType::Plus1Plus1, 1),
    ];
    expected.sort();
    assert_eq!(removed, expected);
}

/// R2: Oblivion Stone's second ability destroys every unmarked nonland permanent,
/// THEN removes the fate counters from all permanents (CR 608.2c: in printed
/// order — removing first would destroy everything). No target is asked for, and
/// the removal takes only fate counters.
#[test]
fn oblivion_stone_destroys_unmarked_then_clears_every_fate_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stone = scenario
        .add_artifact_from_oracle(P0, "Oblivion Stone", OBLIVION_STONE)
        .id();
    let marked = scenario.add_creature(P0, "Marked Bear", 2, 2).id();
    scenario.with_counter(marked, fate(), 1);
    let unmarked = scenario.add_creature(P0, "Unmarked Bear", 2, 2).id();
    let their_relic = scenario
        .add_artifact_from_oracle(P1, "Their Relic", "")
        .id();
    scenario.with_counter(their_relic, fate(), 1);
    let their_bear = scenario.add_creature(P1, "Their Bear", 2, 2).id();
    scenario.with_counter(their_bear, fate(), 1);
    scenario.with_counter(their_bear, CounterType::Plus1Plus1, 1);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.with_counter(land, fate(), 1);
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    // Reach guard: the marks are in place.
    for id in [marked, their_relic, their_bear, land] {
        assert_eq!(count_of(&runner, id, &fate()), 1);
    }

    let outcome = runner.activate(stone, 1).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the ability resolves without a target prompt: {:?}",
        outcome.final_waiting_for()
    );

    assert_eq!(runner.state().objects[&unmarked].zone, Zone::Graveyard);
    for id in [marked, their_relic, their_bear, land] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "{id:?} survives"
        );
        assert_eq!(
            count_of(&runner, id, &fate()),
            0,
            "{id:?} loses its fate counter"
        );
    }
    assert_eq!(
        count_of(&runner, their_bear, &CounterType::Plus1Plus1),
        1,
        "only fate counters are removed"
    );
}

/// R3: Aurification's leaves-the-battlefield trigger removes gold counters from
/// every creature of both players, and only from creatures.
#[test]
fn aurification_leaving_clears_gold_from_every_creature_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aurification = scenario
        .add_enchantment_from_oracle(P0, "Aurification", AURIFICATION)
        .id();
    let mine = scenario.add_creature(P0, "My Bear", 2, 2).id();
    scenario.with_counter(mine, gold(), 1);
    scenario.with_counter(mine, CounterType::Plus1Plus1, 1);
    let theirs = scenario.add_creature(P1, "Their Bear", 2, 2).id();
    scenario.with_counter(theirs, gold(), 2);
    let relic = scenario
        .add_artifact_from_oracle(P1, "Gilded Relic", "")
        .id();
    scenario.with_counter(relic, gold(), 1);
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, "Destroy target enchantment.")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    for (id, n) in [(mine, 1), (theirs, 2), (relic, 1)] {
        assert_eq!(count_of(&runner, id, &gold()), n, "reach guard");
    }

    runner
        .cast(disenchant)
        .target_object(aurification)
        .resolve();
    if !runner.state().stack.is_empty() {
        runner.advance_until_stack_empty();
    }
    assert!(
        runner.state().stack.is_empty(),
        "the leaves-the-battlefield trigger resolved: {:?}",
        runner.state().waiting_for
    );

    assert_eq!(runner.state().objects[&aurification].zone, Zone::Graveyard);
    assert_eq!(count_of(&runner, mine, &gold()), 0);
    assert_eq!(count_of(&runner, theirs, &gold()), 0);
    assert_eq!(
        count_of(&runner, mine, &CounterType::Plus1Plus1),
        1,
        "only gold counters are removed"
    );
    assert_eq!(
        count_of(&runner, relic, &gold()),
        1,
        "a non-creature permanent is outside the population"
    );
}

/// R5: Pestilent Haze's second mode removes two loyalty counters from each
/// planeswalker. A planeswalker with one loyalty loses the one it has (CR 609.3)
/// and is put into its owner's graveyard (CR 704.5i); one with five ends at three.
#[test]
fn pestilent_haze_removes_two_loyalty_from_each_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let weak = scenario
        .add_planeswalker_from_oracle(P0, "Weak Walker", "Jace", 1, "")
        .id();
    let strong = scenario
        .add_planeswalker_from_oracle(P1, "Strong Walker", "Chandra", 5, "")
        .id();
    let haze = scenario
        .add_spell_to_hand_from_oracle(P0, "Pestilent Haze", false, PESTILENT_HAZE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    assert_eq!(count_of(&runner, weak, &CounterType::Loyalty), 1);
    assert_eq!(count_of(&runner, strong, &CounterType::Loyalty), 5);

    let outcome = runner.cast(haze).modes(&[1]).resolve();

    assert_eq!(runner.state().objects[&weak].zone, Zone::Graveyard);
    assert_eq!(count_of(&runner, strong, &CounterType::Loyalty), 3);
    let mut removed = removals(outcome.events());
    removed.sort();
    let mut expected = vec![
        (weak, CounterType::Loyalty, 1),
        (strong, CounterType::Loyalty, 2),
    ];
    expected.sort();
    assert_eq!(removed, expected, "no removal exceeds what was present");
    assert_eq!(runner.state().last_effect_count, Some(3));
}

/// R7: Heartmender's upkeep trigger removes one -1/-1 counter from each creature
/// its controller controls ("you" binds the ability's controller, CR 109.5) and
/// leaves the opponent's creatures alone, with no target prompt.
#[test]
fn heartmender_upkeep_removes_one_counter_from_each_of_your_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.with_library_top(P0, &["P0 Top A", "P0 Top B", "P0 Top C"]);
    scenario.with_library_top(P1, &["P1 Top A", "P1 Top B", "P1 Top C"]);
    scenario.at_phase(Phase::End);
    // Heartmender is P1's, so the drive from P0's end step reaches P1's upkeep.
    let heartmender = scenario
        .add_creature_from_oracle(P1, "Heartmender", 2, 2, HEARTMENDER)
        .id();
    scenario.with_counter(heartmender, CounterType::Minus1Minus1, 1);
    let ally = scenario.add_creature(P1, "Ally Bear", 3, 3).id();
    scenario.with_counter(ally, CounterType::Minus1Minus1, 2);
    let foe = scenario.add_creature(P0, "Foe Bear", 3, 3).id();
    scenario.with_counter(foe, CounterType::Minus1Minus1, 1);
    let mut runner = scenario.build();

    runner.advance_to_phase(Phase::Draw);
    for _ in 0..40 {
        let state = runner.state();
        if state.phase == Phase::Draw && state.active_player == P1 {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("Heartmender's trigger must not prompt: {other:?}"),
        }
    }
    let state = runner.state();
    assert!(
        state.phase == Phase::Draw && state.active_player == P1,
        "the drive must reach P1's draw step; phase={:?} active={:?}",
        state.phase,
        state.active_player
    );

    assert_eq!(
        count_of(&runner, heartmender, &CounterType::Minus1Minus1),
        0
    );
    assert_eq!(count_of(&runner, ally, &CounterType::Minus1Minus1), 1);
    assert_eq!(
        count_of(&runner, foe, &CounterType::Minus1Minus1),
        1,
        "the opponent's creature is not one you control"
    );
}

const PROTEAN_HYDRA: &str = "This creature enters with X +1/+1 counters on it.\n\
If damage would be dealt to this creature, prevent that damage and remove that many +1/+1 counters from it.\n\
Whenever a +1/+1 counter is removed from this creature, put two +1/+1 counters on it at the beginning of the next end step.";
const GLORIOUS_ANTHEM: &str = "Creatures you control get +1/+1.";
const THRULL_PARASITE: &str = "{T}, Pay 2 life: Remove a counter from target nonland permanent.";

/// F1 (CR 603.2c): one `CounterRemoved { count: 2 }` event holds two occurrences,
/// so Protean Hydra's "Whenever a +1/+1 counter is removed from this creature"
/// fires twice (ruling: "If multiple +1/+1 counters are removed from Protean
/// Hydra at once, its last ability will trigger that many times"). Glorious
/// Anthem keeps the 0/0 Hydra alive once Aether Snap strips its two counters;
/// each firing creates its own delayed trigger, restoring 2 + 2 counters.
#[test]
fn aether_snap_triggers_protean_hydra_once_per_counter_removed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature_from_oracle(P0, "Protean Hydra", 0, 0, PROTEAN_HYDRA)
        .id();
    scenario.with_counter(hydra, CounterType::Plus1Plus1, 2);
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM);
    let snap = scenario
        .add_spell_to_hand_from_oracle(P0, "Aether Snap", false, AETHER_SNAP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(snap).resolve();
    let removed = removals(outcome.events());
    assert_eq!(
        removed,
        vec![(hydra, CounterType::Plus1Plus1, 2)],
        "the mass removal emits one aggregate event for the two counters"
    );
    assert_eq!(count_of(&runner, hydra, &CounterType::Plus1Plus1), 0);
    assert!(
        runner.state().objects.contains_key(&hydra)
            && runner.state().objects[&hydra].zone == Zone::Battlefield,
        "Glorious Anthem keeps the Hydra alive"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "one Hydra firing per counter removed (CR 603.2c), each creating its own end-step delayed trigger"
    );
    drive_to_settled_end_step(&mut runner);
    assert_eq!(
        count_of(&runner, hydra, &CounterType::Plus1Plus1),
        4,
        "two delayed triggers each put two +1/+1 counters on the Hydra"
    );
}

/// Drives the current turn from its main phase to its end step: declines to
/// attack, passes priority, and orders simultaneously fired triggers (CR
/// 603.3b; the Hydra's delayed triggers are identical, so any order is the
/// same). Stops once the end step's stack is empty with no delayed trigger
/// still waiting, or at cleanup, whatever the counters on the board.
fn drive_to_settled_end_step(runner: &mut GameRunner) {
    for _ in 0..80 {
        let state = runner.state();
        let settled_end_step = state.phase == Phase::End
            && state.stack.is_empty()
            && state.delayed_triggers.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. });
        if settled_end_step || state.phase == Phase::Cleanup {
            break;
        }
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt before the end step: {other:?}"),
        }
    }
    let state = runner.state();
    assert!(
        matches!(state.phase, Phase::End | Phase::Cleanup),
        "the drive must reach the end step; phase={:?}",
        state.phase
    );
}

const BOON_OF_SAFETY: &str = "Put a shield counter on target creature. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)\nScry 1.";

/// Protean Hydra (with `plus1` +1/+1 counters) kept alive by Glorious Anthem,
/// given two shield counters by two Boon of Safety casts paid with real {W},
/// then Aether Snap is cast. Returns the runner, the Hydra and the removal
/// events of the Aether Snap resolution, after its triggers have resolved.
fn hydra_with_two_shields_under_aether_snap(
    plus1: u32,
) -> (GameRunner, ObjectId, Vec<(ObjectId, CounterType, u32)>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature_from_oracle(P0, "Protean Hydra", 0, 0, PROTEAN_HYDRA)
        .id();
    if plus1 > 0 {
        scenario.with_counter(hydra, CounterType::Plus1Plus1, plus1);
    }
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM);
    let boons: Vec<ObjectId> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Boon of Safety", true, BOON_OF_SAFETY)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 0,
                })
                .id()
        })
        .collect();
    let snap = scenario
        .add_spell_to_hand_from_oracle(P0, "Aether Snap", false, AETHER_SNAP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    for boon in boons {
        runner.cast(boon).target_object(hydra).resolve();
    }
    // Reach guard: both kinds are on the Hydra before the mass removal.
    assert_eq!(count_of(&runner, hydra, &CounterType::Shield), 2);
    assert_eq!(count_of(&runner, hydra, &CounterType::Plus1Plus1), plus1);

    let outcome = runner.cast(snap).resolve();
    let removed = removals(outcome.events());
    runner.advance_until_stack_empty();
    assert_eq!(count_of(&runner, hydra, &CounterType::Shield), 0);
    assert_eq!(count_of(&runner, hydra, &CounterType::Plus1Plus1), 0);
    assert_eq!(
        runner.state().objects[&hydra].zone,
        Zone::Battlefield,
        "Glorious Anthem keeps the Hydra alive"
    );
    (runner, hydra, removed)
}

/// F1 (CR 122.1 + CR 603.2c): Protean Hydra's "Whenever a +1/+1 counter is
/// removed from this creature" fires once per +1/+1 counter removed, and never
/// for a removed counter of another kind. With two +1/+1 and two shield
/// counters, Aether Snap removes all four but the Hydra fires twice, so the
/// next end step restores four +1/+1 counters, not eight.
#[test]
fn aether_snap_triggers_protean_hydra_only_for_its_plus_one_counters() {
    let (mut runner, hydra, removed) = hydra_with_two_shields_under_aether_snap(2);
    assert!(
        removed.contains(&(hydra, CounterType::Shield, 2)),
        "{removed:?}"
    );
    assert!(
        removed.contains(&(hydra, CounterType::Plus1Plus1, 2)),
        "{removed:?}"
    );
    let plus1_removed: u32 = removed
        .iter()
        .filter(|(id, kind, _)| *id == hydra && *kind == CounterType::Plus1Plus1)
        .map(|(_, _, n)| n)
        .sum();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        2,
        "one Hydra firing per +1/+1 counter removed; the shield removals fire nothing"
    );
    assert_eq!(
        runner.state().delayed_triggers.len() as u32,
        plus1_removed,
        "the firings match the +1/+1 counters removed"
    );

    drive_to_settled_end_step(&mut runner);
    assert_eq!(
        count_of(&runner, hydra, &CounterType::Plus1Plus1),
        4,
        "two delayed triggers each put two +1/+1 counters on the Hydra"
    );
}

/// F1 (CR 122.1): removing only shield counters from Protean Hydra is not "a
/// +1/+1 counter is removed", so it never fires and nothing returns.
#[test]
fn aether_snap_removing_only_shields_never_triggers_protean_hydra() {
    let (mut runner, hydra, removed) = hydra_with_two_shields_under_aether_snap(0);
    assert_eq!(
        removed,
        vec![(hydra, CounterType::Shield, 2)],
        "reach guard: the shield counters are removed"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        0,
        "a shield removal is not a +1/+1 removal"
    );

    drive_to_settled_end_step(&mut runner);
    assert_eq!(runner.state().phase, Phase::End, "the end step is reached");
    assert_eq!(count_of(&runner, hydra, &CounterType::Plus1Plus1), 0);
}

/// F2 (CR 608.2d): a finite untyped single removal ("Remove a counter from target
/// nonland permanent", Thrull Parasite) removes exactly one counter, of one of
/// the kinds present. Which kind is left to the counters' read order (the
/// ordering rule itself is unit-tested beside `order_untyped_removal_kinds`),
/// so this asserts only the invariants every run satisfies.
#[test]
fn thrull_parasite_finite_removal_removes_exactly_one_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let parasite = scenario
        .add_creature_from_oracle(P0, "Thrull Parasite", 1, 1, THRULL_PARASITE)
        .id();
    let victim = scenario.add_creature(P1, "Their Bear", 3, 3).id();
    scenario.with_counter(victim, CounterType::Plus1Plus1, 1);
    scenario.with_counter(victim, ice(), 1);
    let mut runner = scenario.build();
    // Reach guard: both kinds are present before the activation.
    assert_eq!(count_of(&runner, victim, &CounterType::Plus1Plus1), 1);
    assert_eq!(count_of(&runner, victim, &ice()), 1);

    runner.activate(parasite, 0).target_object(victim).resolve();
    let plus1 = count_of(&runner, victim, &CounterType::Plus1Plus1);
    let ice_left = count_of(&runner, victim, &ice());
    assert_eq!(plus1 + ice_left, 1, "exactly one counter is removed");
    assert!(
        (plus1, ice_left) == (1, 0) || (plus1, ice_left) == (0, 1),
        "the survivor is one of the two kinds: +1/+1={plus1}, ice={ice_left}"
    );
}

const VOREL_OF_THE_HULL_CLADE: &str =
    "{G}{U}, {T}: Double the number of each kind of counter on target artifact, creature, or land.";
const ASTRAL_CORNUCOPIA: &str = "This artifact enters with X charge counters on it.\n\
{T}: Choose a color. Add one mana of that color for each charge counter on this artifact.";
const FINAL_ACT: &str = "Choose one or more \u{2014}\n\
\u{2022} Destroy all creatures.\n\
\u{2022} Destroy all planeswalkers.\n\
\u{2022} Destroy all battles.\n\
\u{2022} Exile all graveyards.\n\
\u{2022} Each opponent loses all counters.";
const PLATINUM_ANGEL: &str =
    "Flying\nYou can't lose the game and your opponents can't win the game.";

/// A board with Vorel of the Hull Clade and `cornucopias` Astral Cornucopias,
/// each holding one charge counter, plus Aether Snap in hand at {0}.
///
/// Scaffolding (not under test): the single charge counter stands in for
/// casting a Cornucopia with X = 1, and `double_with_vorel` untaps Vorel and
/// refills {G}{U} between activations. Every doubling is a real activation.
fn cornucopia_board(cornucopias: usize) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vorel = scenario
        .add_creature_from_oracle(P0, "Vorel of the Hull Clade", 1, 4, VOREL_OF_THE_HULL_CLADE)
        .id();
    let relics: Vec<ObjectId> = (0..cornucopias)
        .map(|_| {
            scenario
                .add_artifact_from_oracle(P0, "Astral Cornucopia", ASTRAL_CORNUCOPIA)
                .id()
        })
        .collect();
    for relic in &relics {
        scenario.with_counter(*relic, charge(), 1);
    }
    let snap = scenario
        .add_spell_to_hand_from_oracle(P0, "Aether Snap", false, AETHER_SNAP)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (scenario.build(), vorel, relics, snap)
}

/// CR 701.10e: each Vorel activation gives the target as many of each kind of
/// counter as it already has. Asserts the exact count after every doubling, so
/// a silently dropped addition is caught at the step where it happens.
fn double_with_vorel(runner: &mut GameRunner, vorel: ObjectId, target: ObjectId, doublings: u32) {
    for k in 1..=doublings {
        {
            let state = runner.state_mut();
            state.objects.get_mut(&vorel).unwrap().tapped = false;
            for mana in [ManaType::Green, ManaType::Blue] {
                let _ = state.add_mana_to_pool(P0, ManaUnit::new(mana, ObjectId(0), false, vec![]));
            }
        }
        runner.activate(vorel, 0).target_object(target).resolve();
        assert_eq!(
            count_of(runner, target, &charge()),
            1 << k,
            "after doubling {k}"
        );
    }
}

/// F2 witness (PR #10814): two Astral Cornucopias doubled to 2^31 charge
/// counters each by 31 real Vorel activations apiece, then Aether Snap. The
/// removal total (2^32) does not fit `u32` and the counters removed this way
/// do not fit `i32`; neither aggregate may wrap or panic. Both Cornucopias end
/// with no counters and each removal event carries its true count.
#[test]
fn aether_snap_clears_two_cornucopias_at_two_to_the_thirty_one() {
    let (mut runner, vorel, relics, snap) = cornucopia_board(2);
    for relic in &relics {
        double_with_vorel(&mut runner, vorel, *relic, 31);
    }
    for relic in &relics {
        assert_eq!(count_of(&runner, *relic, &charge()), 2_147_483_648);
    }

    let outcome = runner.cast(snap).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves: {:?}",
        outcome.final_waiting_for()
    );
    let mut removed = removals(outcome.events());
    removed.sort_by_key(|(id, _, _)| *id);
    let mut expected: Vec<_> = relics
        .iter()
        .map(|id| (*id, charge(), 1u32 << 31))
        .collect();
    expected.sort_by_key(|(id, _, _)| *id);
    assert_eq!(removed, expected, "true counts, one event per Cornucopia");
    for relic in &relics {
        assert_eq!(count_of(&runner, *relic, &charge()), 0);
    }
}

/// Control for the witness: one Cornucopia at 2^31 fits every aggregate.
#[test]
fn aether_snap_clears_one_cornucopia_at_two_to_the_thirty_one() {
    let (mut runner, vorel, relics, snap) = cornucopia_board(1);
    double_with_vorel(&mut runner, vorel, relics[0], 31);

    let outcome = runner.cast(snap).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        removals(outcome.events()),
        vec![(relics[0], charge(), 1u32 << 31)]
    );
    assert_eq!(count_of(&runner, relics[0], &charge()), 0);
}

/// Boundary seed (scaffolding: `u32::MAX` charge counters placed directly):
/// one permanent's removals total `u32::MAX + 1`.
#[test]
fn aether_snap_clears_a_full_counter_stack_plus_one() {
    let (mut runner, _vorel, relics, snap) = cornucopia_board(1);
    {
        let counters = &mut runner
            .state_mut()
            .objects
            .get_mut(&relics[0])
            .unwrap()
            .counters;
        counters.insert(charge(), u32::MAX);
        counters.insert(ice(), 1);
    }

    let outcome = runner.cast(snap).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let mut removed = removals(outcome.events());
    removed.sort_by_key(|(_, _, count)| *count);
    assert_eq!(
        removed,
        vec![(relics[0], ice(), 1), (relics[0], charge(), u32::MAX)]
    );
    assert!(runner.state().objects[&relics[0]].counters.is_empty());
}

/// `PlayerCounterChanged` deltas for `player`'s `kind`, in emission order.
fn player_counter_deltas(
    events: &[GameEvent],
    player: PlayerId,
    kind: PlayerCounterKind,
) -> Vec<i32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerCounterChanged {
                player: changed,
                counter_kind,
                delta,
            } if *changed == player && *counter_kind == kind => Some(*delta),
            _ => None,
        })
        .collect()
}

/// CR 122.1: Final Act's "Each opponent loses all counters" removes player
/// counter kinds holding more than `i32::MAX` counters. Poison counters past
/// ten exist only under a can't-lose effect (CR 104.3d), so the opponent
/// controls Platinum Angel and "Destroy all creatures" is not chosen.
/// Scaffolding: the large counts are placed directly (a real add is capped at
/// `i32::MAX` per edit and accumulates across adds).
#[test]
fn final_act_removes_player_counter_kinds_above_i32_max() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Platinum Angel", 4, 4, PLATINUM_ANGEL);
    let final_act = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Act", false, FINAL_ACT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let opponent = &mut state.players[1];
        opponent.poison_counters = 1 << 31;
        opponent
            .player_counters
            .insert(PlayerCounterKind::Experience, 3_000_000_000);
        let caster = &mut state.players[0];
        caster.poison_counters = 1;
        caster
            .player_counters
            .insert(PlayerCounterKind::Experience, 2);
    }
    // Reach guard: Platinum Angel keeps the poisoned opponent in the game.
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut Vec::new());
    assert!(!runner.state().players[1].is_eliminated);

    let outcome = runner.cast(final_act).modes(&[4]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves: {:?}",
        outcome.final_waiting_for()
    );

    let opponent = &runner.state().players[1];
    assert!(!opponent.is_eliminated);
    assert_eq!(opponent.poison_counters, 0);
    assert!(
        opponent.player_counters.is_empty(),
        "{:?}",
        opponent.player_counters
    );
    let events = outcome.events();
    assert_eq!(
        player_counter_deltas(events, P1, PlayerCounterKind::Experience),
        vec![-i32::MAX, -852_516_353]
    );
    assert_eq!(
        player_counter_deltas(events, P1, PlayerCounterKind::Poison),
        vec![-i32::MAX, -1]
    );
    // Final Act affects only opponents.
    let caster = &runner.state().players[0];
    assert_eq!(caster.poison_counters, 1);
    assert_eq!(
        caster.player_counters.get(&PlayerCounterKind::Experience),
        Some(&2)
    );
    assert!(player_counter_deltas(events, P0, PlayerCounterKind::Experience).is_empty());
}

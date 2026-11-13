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
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
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
    // Drive to the end step: decline to attack, then pass priority until both
    // delayed triggers have fired and resolved.
    for _ in 0..60 {
        if count_of(&runner, hydra, &CounterType::Plus1Plus1) == 4 {
            break;
        }
        match runner.state().waiting_for.clone() {
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
            // Two identical Hydra delayed triggers fire together: the
            // controller orders them (CR 603.3b); any order is the same.
            WaitingFor::OrderTriggers { triggers, .. } => {
                assert_eq!(triggers.len(), 2, "both Hydra delayed triggers are ordered");
                runner
                    .act(GameAction::OrderTriggers { order: vec![0, 1] })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt before the end step: {other:?}"),
        }
    }
    assert_eq!(
        count_of(&runner, hydra, &CounterType::Plus1Plus1),
        4,
        "two delayed triggers each put two +1/+1 counters on the Hydra"
    );
}

/// F2 (CR 608.2d): a finite untyped single removal ("Remove a counter from target
/// nonland permanent", Thrull Parasite) is not remove-all processing, so the
/// `CounterType` ordering of the mass resolver must not decide which kind it
/// removes. Every run removes exactly one counter in total, and across runs
/// (each object's `HashMap` is independently seeded) both kinds are removed at
/// least once; a fixed lowest-kind pick would remove only the +1/+1 counter.
#[test]
fn thrull_parasite_finite_removal_is_not_pinned_to_the_lowest_counter_kind() {
    let mut removed_plus1 = 0u32;
    let mut removed_ice = 0u32;
    for _ in 0..24 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let parasite = scenario
            .add_creature_from_oracle(P0, "Thrull Parasite", 1, 1, THRULL_PARASITE)
            .id();
        let victim = scenario.add_creature(P1, "Their Bear", 3, 3).id();
        scenario.with_counter(victim, CounterType::Plus1Plus1, 1);
        scenario.with_counter(victim, ice(), 1);
        let mut runner = scenario.build();

        runner.activate(parasite, 0).target_object(victim).resolve();
        let plus1 = count_of(&runner, victim, &CounterType::Plus1Plus1);
        let ice_left = count_of(&runner, victim, &ice());
        assert_eq!(plus1 + ice_left, 1, "exactly one counter is removed");
        removed_plus1 += u32::from(plus1 == 0);
        removed_ice += u32::from(ice_left == 0);
    }
    assert!(
        removed_plus1 > 0 && removed_ice > 0,
        "both kinds are removable by a finite single removal: +1/+1 removed {removed_plus1}x, ice removed {removed_ice}x"
    );
}

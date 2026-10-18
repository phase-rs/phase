//! The M13 Ring cycle: "At the beginning of your upkeep, put a +1/+1 counter on
//! equipped creature if it's <color>."
//!
//! Oracle (Ring of Valkas; the other four Rings differ only in their first line
//! and the color word):
//! > Equipped creature has haste. (It can attack and {T} no matter when it came
//! > under your control.)
//! > At the beginning of your upkeep, put a +1/+1 counter on equipped creature
//! > if it's red.
//! > Equip {1} ({1}: Attach to target creature you control. Equip only as a
//! > sorcery.)
//!
//! Reading: the "if" does not immediately follow the trigger condition, so it is
//! not an intervening "if" (CR 603.4). The ability always triggers, and the
//! condition is checked only as it resolves (CR 608.2c), where "it" is the
//! equipped creature: whatever creature the Ring is attached to at that moment
//! (CR 301.5a + CR 301.5f), whoever controls it (CR 301.5d). It is not a target,
//! because the text never says "target" (CR 115.10a).
//!
//! The defect: the condition looked for "it" among the ability's targets and
//! then the trigger event's subject. An upkeep trigger has neither, so the
//! condition was always false and no counter was ever placed, even though the
//! counter effect itself found the equipped creature.
//!
//! Every test drives P0's upkeep through the real turn machinery (CR 503.1a) and
//! resolves the Ring's trigger by passing priority. The reach guard proves the
//! trigger reached the stack, so a zero-counter assertion is never vacuous.

use std::collections::HashSet;

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::run_combat;

/// One Ring of the cycle: its verbatim Oracle text and the color it checks.
struct Ring {
    name: &'static str,
    oracle: &'static str,
    color: ManaColor,
}

const RING_OF_VALKAS: Ring = Ring {
    name: "Ring of Valkas",
    oracle: "Equipped creature has haste. (It can attack and {T} no matter when it came under your \
             control.)\nAt the beginning of your upkeep, put a +1/+1 counter on equipped creature if \
             it's red.\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)",
    color: ManaColor::Red,
};

const RING_CYCLE: [Ring; 5] = [
    RING_OF_VALKAS,
    Ring {
        name: "Ring of Evos Isle",
        oracle: "{2}: Equipped creature gains hexproof until end of turn. (It can't be the target of \
                 spells or abilities your opponents control.)\nAt the beginning of your upkeep, put a \
                 +1/+1 counter on equipped creature if it's blue.\nEquip {1} ({1}: Attach to target \
                 creature you control. Equip only as a sorcery.)",
        color: ManaColor::Blue,
    },
    Ring {
        name: "Ring of Kalonia",
        oracle: "Equipped creature has trample. (It can deal excess combat damage to the player or \
                 planeswalker it's attacking.)\nAt the beginning of your upkeep, put a +1/+1 counter \
                 on equipped creature if it's green.\nEquip {1} ({1}: Attach to target creature you \
                 control. Equip only as a sorcery.)",
        color: ManaColor::Green,
    },
    Ring {
        name: "Ring of Thune",
        oracle: "Equipped creature has vigilance. (Attacking doesn't cause it to tap.)\nAt the \
                 beginning of your upkeep, put a +1/+1 counter on equipped creature if it's white.\n\
                 Equip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)",
        color: ManaColor::White,
    },
    Ring {
        name: "Ring of Xathrid",
        oracle: "{2}: Regenerate equipped creature. (The next time that creature would be destroyed \
                 this turn, instead tap it, remove it from combat, and heal all damage on it.)\nAt \
                 the beginning of your upkeep, put a +1/+1 counter on equipped creature if it's \
                 black.\nEquip {1} ({1}: Attach to target creature you control. Equip only as a \
                 sorcery.)",
        color: ManaColor::Black,
    },
];

const OBEKA_ORACLE: &str = "Menace\nWhenever Obeka deals combat damage to a player, \
you get that many additional upkeep steps after this phase.";

/// A 2/2 creature in a fixture: who controls it and what colors it is.
type CreatureSpec = (PlayerId, Vec<ManaColor>);

/// Puts `ring` on P0's battlefield next to one 2/2 per `creatures` entry,
/// attaches the Ring to `creatures[equipped]` when given, then runs P0's upkeep
/// and resolves everything it put on the stack. Returns the runner and the
/// creature ids in `creatures` order.
fn run_upkeep(
    ring: &Ring,
    creatures: &[CreatureSpec],
    equipped: Option<usize>,
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    // Every fixture keeps the Equipment subtype: without it the unattached
    // case would not be the real "Equipment with no host" state.
    let ring_id = scenario
        .add_artifact_from_oracle(P0, ring.name, ring.oracle)
        .with_subtypes(vec!["Equipment"])
        .id();
    let creature_ids: Vec<ObjectId> = creatures
        .iter()
        .enumerate()
        .map(|(index, (controller, colors))| {
            scenario
                .add_creature(*controller, &format!("Creature {index}"), 2, 2)
                .with_color(colors.clone())
                .id()
        })
        .collect();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Plains");
    }
    let mut runner = scenario.build();
    if let Some(index) = equipped {
        attach_to(runner.state_mut(), ring_id, creature_ids[index]);
    }
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), ring_id);

    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().active_player, P0);
    assert!(
        ring_trigger_entry(&runner, ring_id).is_some(),
        "reach guard: {} triggered at P0's upkeep, stack {:?}",
        ring.name,
        runner.stack_names()
    );
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            return (runner, creature_ids);
        }
        runner
            .act(GameAction::PassPriority)
            .expect("each player passes");
    }
    panic!("the upkeep stack never drained: {:?}", runner.stack_names());
}

/// The stack entry id of a triggered ability from `ring`, if one is on the stack.
fn ring_trigger_entry(runner: &GameRunner, ring: ObjectId) -> Option<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ring => {
                Some(entry.id)
            }
            _ => None,
        })
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 608.2c + CR 301.5a: a red equipped creature gets exactly one counter.
/// Revert canary: before the fix the condition never found "it" and this was 0.
#[test]
fn ring_of_valkas_red_host_gets_one_counter_at_upkeep() {
    let (runner, creatures) = run_upkeep(&RING_OF_VALKAS, &[(P0, vec![ManaColor::Red])], Some(0));

    assert_eq!(plus_counters(&runner, creatures[0]), 1);
}

/// A green equipped creature gets nothing. The red sibling above, through the
/// same harness, is the positive half of this pair.
#[test]
fn ring_of_valkas_nonred_host_gets_no_counter() {
    let (runner, creatures) = run_upkeep(&RING_OF_VALKAS, &[(P0, vec![ManaColor::Green])], Some(0));

    assert_eq!(plus_counters(&runner, creatures[0]), 0);
}

/// The fix is for the class: every Ring of the cycle counts its own color and
/// ignores a creature of another color.
#[test]
fn every_m13_ring_checks_its_own_color_on_the_equipped_creature() {
    for (index, ring) in RING_CYCLE.iter().enumerate() {
        let other_color = RING_CYCLE[(index + 1) % RING_CYCLE.len()].color;

        let (runner, creatures) = run_upkeep(ring, &[(P0, vec![ring.color])], Some(0));
        assert_eq!(
            plus_counters(&runner, creatures[0]),
            1,
            "{} on a {:?} creature",
            ring.name,
            ring.color
        );

        let (runner, creatures) = run_upkeep(ring, &[(P0, vec![other_color])], Some(0));
        assert_eq!(
            plus_counters(&runner, creatures[0]),
            0,
            "{} on a {other_color:?} creature",
            ring.name
        );
    }
}

/// CR 301.5f: "it" is the creature the Ring is attached to, not any red
/// creature. An unequipped red bystander gets nothing.
#[test]
fn ring_of_valkas_counts_only_the_equipped_creature() {
    let (runner, creatures) = run_upkeep(
        &RING_OF_VALKAS,
        &[(P0, vec![ManaColor::Red]), (P0, vec![ManaColor::Red])],
        Some(0),
    );

    assert_eq!(
        plus_counters(&runner, creatures[0]),
        1,
        "the equipped creature"
    );
    assert_eq!(plus_counters(&runner, creatures[1]), 0, "the bystander");
}

/// An unattached Ring has no equipped creature, so "it" has no referent and no
/// creature gets a counter. The trigger still fires: the "if" is not an
/// intervening "if" (CR 603.4), which the reach guard in `run_upkeep` proves.
#[test]
fn unattached_ring_of_valkas_puts_no_counter_on_a_red_creature() {
    let (runner, creatures) = run_upkeep(&RING_OF_VALKAS, &[(P0, vec![ManaColor::Red])], None);

    assert_eq!(plus_counters(&runner, creatures[0]), 0);
}

/// CR 301.5d: the equipped creature's controller is separate from the
/// Equipment's. P0's Ring on P1's red creature still counts it at P0's upkeep.
#[test]
fn ring_of_valkas_counts_an_equipped_creature_another_player_controls() {
    let (runner, creatures) = run_upkeep(&RING_OF_VALKAS, &[(P1, vec![ManaColor::Red])], Some(0));

    assert_eq!(plus_counters(&runner, creatures[0]), 1);
}

/// CR 105.2b: a multicolored creature that includes red is red.
#[test]
fn ring_of_valkas_counts_a_multicolored_red_creature() {
    let (runner, creatures) = run_upkeep(
        &RING_OF_VALKAS,
        &[(P0, vec![ManaColor::Blue, ManaColor::Red])],
        Some(0),
    );

    assert_eq!(plus_counters(&runner, creatures[0]), 1);
}

/// What the turn looked like after Obeka's combat, through the postcombat main
/// phase.
struct AfterObekaCombat {
    runner: GameRunner,
    obeka: ObjectId,
    other: ObjectId,
    /// The step in progress when combat's triggers finished, then every step
    /// entered afterwards, ending with `PostCombatMain`.
    steps: Vec<Phase>,
    /// How many distinct Ring of Valkas triggers reached the stack.
    ring_triggers: usize,
}

/// Obeka (2 power, blue-black-red) attacks unblocked with Ring of Valkas on
/// `equip`, which is Obeka or a green creature P0 also controls.
fn obeka_combat(equip_obeka: bool) -> AfterObekaCombat {
    let mut scenario = GameScenario::new();
    // Main phase 1: this turn's own upkeep has already happened.
    scenario.at_phase(Phase::PreCombatMain);
    let obeka = scenario
        .add_creature_from_oracle(P0, "Obeka, Splitter of Seconds", 2, 5, OBEKA_ORACLE)
        .with_color(vec![ManaColor::Blue, ManaColor::Black, ManaColor::Red])
        .id();
    let other = scenario
        .add_creature(P0, "Green Bear", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let ring = scenario
        .add_artifact_from_oracle(P0, RING_OF_VALKAS.name, RING_OF_VALKAS.oracle)
        .with_subtypes(vec!["Equipment"])
        .id();
    scenario.add_card_to_library_top(P0, "Island");
    scenario.add_card_to_library_top(P0, "Island");
    let mut runner = scenario.build();
    let host = if equip_obeka { obeka } else { other };
    attach_to(runner.state_mut(), ring, host);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), ring);

    run_combat(&mut runner, vec![obeka], vec![]);
    runner.advance_until_stack_empty();

    let turn = runner.state().turn_number;
    let mut steps = vec![runner.state().phase];
    let mut ring_trigger_ids = HashSet::new();
    for _ in 0..40 {
        ring_trigger_ids.extend(ring_trigger_entry(&runner, ring));
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority should succeed");
        steps.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
        assert_eq!(runner.state().turn_number, turn, "{steps:?}");
        if steps.last() == Some(&Phase::PostCombatMain) {
            return AfterObekaCombat {
                runner,
                obeka,
                other,
                steps,
                ring_triggers: ring_trigger_ids.len(),
            };
        }
    }
    panic!("did not reach the postcombat main phase within 40 passes: {steps:?}");
}

/// The steps from the end of combat onwards.
fn from_end_of_combat(steps: &[Phase]) -> &[Phase] {
    let start = steps
        .iter()
        .position(|phase| *phase == Phase::EndCombat)
        .unwrap_or_else(|| panic!("end of combat never ran: {steps:?}"));
    &steps[start..]
}

/// CR 500.10 + CR 500.11 + CR 503.1a: Obeka's 2 combat damage creates two
/// upkeep steps; Ring of Valkas triggers in each and red Obeka gets a counter
/// each time.
#[test]
fn obeka_wearing_ring_of_valkas_gets_a_counter_in_each_created_upkeep() {
    let after = obeka_combat(true);

    assert_eq!(
        from_end_of_combat(&after.steps),
        [
            Phase::EndCombat,
            Phase::Upkeep,
            Phase::Upkeep,
            Phase::PostCombatMain
        ],
        "reach guard: two created upkeeps, {:?}",
        after.steps
    );
    assert_eq!(
        after.ring_triggers, 2,
        "reach guard: one trigger per upkeep"
    );
    assert_eq!(plus_counters(&after.runner, after.obeka), 2);
}

/// The same two created upkeeps with the Ring on a green creature: the Ring
/// still triggers twice, but neither the green host nor the unequipped red
/// Obeka gets a counter.
#[test]
fn obeka_created_upkeeps_with_ring_of_valkas_on_a_green_creature_place_no_counter() {
    let after = obeka_combat(false);

    assert_eq!(
        from_end_of_combat(&after.steps),
        [
            Phase::EndCombat,
            Phase::Upkeep,
            Phase::Upkeep,
            Phase::PostCombatMain
        ],
        "reach guard: two created upkeeps, {:?}",
        after.steps
    );
    assert_eq!(
        after.ring_triggers, 2,
        "reach guard: one trigger per upkeep"
    );
    assert_eq!(
        plus_counters(&after.runner, after.other),
        0,
        "the green host"
    );
    assert_eq!(
        plus_counters(&after.runner, after.obeka),
        0,
        "unequipped Obeka"
    );
}

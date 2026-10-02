//! Public combat regressions; Oracle verified against MTGJSON AtomicCards.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const HIGH_GROUND: &str = "Each creature you control can block an additional creature each combat.";
const SUSTAINER: &str = "Flying\nWhenever this creature blocks, it gets +0/+2 until end of turn.";
const WALL: &str = "Defender\nWhenever this creature blocks a creature, that creature doesn't untap during its controller's next untap step.";
const SAWJACK: &str = "Reach (This creature can block creatures with flying.)\nWhenever this creature blocks a creature with flying, this creature gets +2/+0 until end of turn.";
const CADETS: &str = "Whenever this creature blocks or becomes blocked, target opponent gains control of it. (This removes this creature from combat.)";

fn priority_to(runner: &mut GameRunner, blockers: bool) {
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) && blockers
            || matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            ) && !blockers
        {
            return;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    panic!("combat prompt not reached");
}

fn declare(runner: &mut GameRunner, attackers: &[ObjectId], sources: &[ObjectId]) {
    priority_to(runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .unwrap();
    priority_to(runner, true);
    if let WaitingFor::DeclareBlockers {
        block_capacities, ..
    } = &runner.state().waiting_for
    {
        for source in sources {
            assert!(
                block_capacities[source].is_none_or(|capacity| capacity >= attackers.len() as u32)
            );
        }
    }
    let assignments: Vec<_> = sources
        .iter()
        .flat_map(|source| attackers.iter().map(move |attacker| (*source, *attacker)))
        .collect();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: assignments.clone(),
        })
        .unwrap();
    let combat = runner.state().combat.as_ref().unwrap();
    for attacker in attackers {
        let actual = combat
            .blocker_assignments
            .get(attacker)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        assert_eq!(actual.len(), sources.len());
        for source in sources {
            assert!(actual.contains(source));
        }
    }
    for source in sources {
        let actual = &combat.blocker_to_attacker[source];
        assert_eq!(actual.len(), attackers.len());
        for attacker in attackers {
            assert!(actual.contains(attacker));
        }
    }
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner.act(GameAction::OrderTriggers { order }).unwrap();
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Player(P0)),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected trigger prompt {other:?}"),
        }
    }
    panic!("trigger placement did not settle");
}

fn setup(
    oracle: &str,
    name: &str,
    count: usize,
    flying: bool,
    mode: TriggerMode,
) -> (GameRunner, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = {
        let mut builder = scenario.add_creature(P0, "First attacker", 1, 1);
        if flying {
            builder.with_keyword(Keyword::Flying);
        }
        builder.id()
    };

    let second = scenario.add_creature(P0, "Second attacker", 1, 1).id();
    let sources: Vec<_> = (0..count)
        .map(|_| {
            scenario
                .add_creature(P1, name, 2, 3)
                .from_oracle_text_with_keywords(
                    if name == "High-Rise Sawjack" {
                        &["Reach"]
                    } else {
                        &[]
                    },
                    oracle,
                )
                .id()
        })
        .collect();
    scenario.add_enchantment_from_oracle(P1, "High Ground", HIGH_GROUND);
    let runner = scenario.build();
    for source in &sources {
        let object = &runner.state().objects[source];
        assert_eq!(object.trigger_definitions.len(), 1);
        assert_eq!(object.trigger_definitions[0].definition.mode, mode);
        assert!(!serde_json::to_string(object)
            .unwrap()
            .contains("Unimplemented"));
    }
    (runner, vec![first, second], sources)
}

fn count(runner: &GameRunner, source: ObjectId) -> usize {
    let count = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count();
    assert!(count > 0, "reach guard: source has actual stack triggers");
    count
}

#[test]
fn bare_blocks_once_for_two_assignments() {
    let (mut runner, attackers, sources) = setup(
        SUSTAINER,
        "Sustainer of the Realm",
        1,
        false,
        TriggerMode::Blocks,
    );
    declare(&mut runner, &attackers, &sources);
    let observed = count(&runner, sources[0]);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    let object = &runner.state().objects[&sources[0]];
    eprintln!(
        "MEASURED bare Sustainer triggers={observed}, resolved P/T={:?}/{:?}",
        object.power, object.toughness
    );
    // CR 509.3a: bare blocks triggers once even when blocking multiple creatures.
    assert_eq!(
        (observed, object.power, object.toughness),
        (1, Some(2), Some(5))
    );
}

#[test]
fn qualified_blocks_once_per_attacker() {
    let (mut runner, attackers, sources) =
        setup(WALL, "Wall of Frost", 1, false, TriggerMode::Blocks);
    declare(&mut runner, &attackers, &sources);
    let observed = count(&runner, sources[0]);
    let mut bound_attackers: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == sources[0])
        .map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                trigger_event: Some(GameEvent::BlockersDeclared { assignments }),
                ..
            } => {
                assert_eq!(assignments.len(), 1);
                assert_eq!(assignments[0].0, sources[0]);
                assignments[0].1
            }
            other => panic!("missing qualified attacker binding: {other:?}"),
        })
        .collect();
    bound_attackers.sort();
    let mut expected = attackers.clone();
    expected.sort();
    assert_eq!(bound_attackers, expected);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    // CR 509.3b: blocks a creature triggers once for each blocked attacker.
    assert_eq!(observed, 2);
}

#[test]
fn qualified_blocks_only_matching_attacker() {
    let (mut runner, attackers, sources) =
        setup(SAWJACK, "High-Rise Sawjack", 1, true, TriggerMode::Blocks);
    declare(&mut runner, &attackers, &sources);
    let observed = count(&runner, sources[0]);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    eprintln!("MEASURED High-Rise Sawjack triggers={observed}");
    // CR 509.3b: only the flying attacker matches this qualified trigger.
    assert_eq!(observed, 1);
    assert_eq!(runner.state().objects[&sources[0]].power, Some(4));
}

#[test]
fn bare_compound_blocks_once_with_player_effect_target() {
    let (mut runner, attackers, sources) = setup(
        CADETS,
        "Goblin Cadets",
        1,
        false,
        TriggerMode::BlocksOrBecomesBlocked,
    );
    assert_eq!(
        runner.state().objects[&sources[0]].trigger_definitions[0]
            .definition
            .valid_target,
        Some(engine::types::ability::TargetFilter::Player)
    );
    declare(&mut runner, &attackers, &sources);
    let observed = count(&runner, sources[0]);
    eprintln!("MEASURED Goblin Cadets triggers={observed}");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    // CR 509.3a: the blocker side of bare blocks or becomes blocked triggers once.
    assert_eq!(observed, 1);
}

#[test]
fn bare_blocks_once_for_each_source() {
    let (mut runner, attackers, sources) = setup(
        SUSTAINER,
        "Sustainer of the Realm",
        2,
        false,
        TriggerMode::Blocks,
    );
    declare(&mut runner, &attackers, &sources);
    let observed: Vec<_> = sources
        .iter()
        .map(|source| count(&runner, *source))
        .collect();
    eprintln!("MEASURED two Sustainers source trigger counts={observed:?}");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    // CR 509.3a: each source independently triggers once for its declaration.
    assert_eq!(observed, vec![1, 1]);
}

#[test]
fn bare_becomes_blocked_preserves_single_trigger_for_two_blockers() {
    const KARN: &str = "Whenever Karn blocks or becomes blocked, it gets -4/+4 until end of turn.\n{1}: Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn.";
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Karn, Silver Golem", 4, 4, KARN)
        .id();
    let blockers: Vec<_> = (0..2)
        .map(|_| scenario.add_creature(P1, "Blocker", 1, 1).id())
        .collect();
    let mut runner = scenario.build();
    let object = &runner.state().objects[&source];
    assert_eq!(
        object.trigger_definitions[0].definition.mode,
        TriggerMode::BlocksOrBecomesBlocked
    );
    assert!(!serde_json::to_string(object)
        .unwrap()
        .contains("Unimplemented"));
    priority_to(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(source, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .unwrap();
    priority_to(&mut runner, true);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: blockers.iter().map(|id| (*id, source)).collect(),
        })
        .unwrap();
    assert_eq!(
        runner.state().combat.as_ref().unwrap().blocker_assignments[&source],
        blockers
    );
    let observed = count(&runner, source);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    let object = &runner.state().objects[&source];
    eprintln!(
        "MEASURED Karn attacker-side triggers={observed}, resolved P/T={:?}/{:?}",
        object.power, object.toughness
    );
    // CR 509.3c: bare becomes blocked triggers once despite multiple blockers.
    assert_eq!(
        (observed, object.power, object.toughness),
        (1, Some(0), Some(8))
    );
}

#[test]
fn bebop_attack_sibling_preserves_single_printed_trigger() {
    const BEBOP: &str = "Whenever Bebop & Rocksteady attack or block, sacrifice a permanent unless you discard a card.";
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Bebop & Rocksteady", 6, 6, BEBOP)
        .id();
    let mut runner = scenario.build();
    let object = &runner.state().objects[&source];
    assert!(object
        .trigger_definitions
        .as_slice()
        .iter()
        .any(|entry| entry.definition.mode == TriggerMode::Attacks));
    assert!(object
        .trigger_definitions
        .as_slice()
        .iter()
        .any(|entry| matches!(entry.definition.mode, TriggerMode::Unknown(_))));
    priority_to(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(source, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .unwrap();
    let observed = count(&runner, source);
    eprintln!(
        "MEASURED Bebop & Rocksteady attack sibling triggers={observed}; block remains Unknown"
    );
    // CR 603.2c: the single declared attacking source triggers its attack ability once.
    assert_eq!(observed, 1);
}

#[test]
fn qualified_blocks_no_matching_attacker() {
    let (mut runner, attackers, sources) =
        setup(SAWJACK, "High-Rise Sawjack", 1, false, TriggerMode::Blocks);
    declare(&mut runner, &attackers, &sources);
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id != sources[0]));
    assert_eq!(runner.state().objects[&sources[0]].power, Some(2));
    // Paired positive fixture proves the qualifier, not the source parse, rejects.
    qualified_blocks_only_matching_attacker();
}

#[test]
fn bare_nonparticipating_source_does_not_fire() {
    let (mut runner, attackers, sources) = setup(
        SUSTAINER,
        "Sustainer of the Realm",
        2,
        false,
        TriggerMode::Blocks,
    );
    declare(&mut runner, &attackers, &sources[..1]);
    assert_eq!(count(&runner, sources[0]), 1);
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id != sources[1]));
}

#[test]
fn bare_empty_assignments_do_not_fire() {
    let (mut runner, attackers, sources) = setup(
        SUSTAINER,
        "Sustainer of the Realm",
        1,
        false,
        TriggerMode::Blocks,
    );
    declare(&mut runner, &attackers, &[]);
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id != sources[0]));
    bare_blocks_once_for_two_assignments();
}

#[test]
fn bare_separate_trigger_definitions_each_fire_once() {
    let (mut runner, attackers, sources) = setup(
        SUSTAINER,
        "Sustainer of the Realm",
        1,
        false,
        TriggerMode::Blocks,
    );
    let object = runner.state_mut().objects.get_mut(&sources[0]).unwrap();
    let definition = object.trigger_definitions[0].definition.clone();
    object.push_printed_trigger(definition);
    declare(&mut runner, &attackers, &sources);
    assert_eq!(count(&runner, sources[0]), 2);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&sources[0]].toughness, Some(7));
}

#[test]
fn bare_player_target_nonparticipating_source_does_not_fire() {
    let (mut runner, attackers, sources) = setup(
        CADETS,
        "Goblin Cadets",
        2,
        false,
        TriggerMode::BlocksOrBecomesBlocked,
    );
    declare(&mut runner, &attackers, &sources[..1]);
    // CR 509.3a: only the creature actually declared as a blocker triggers.
    assert_eq!(count(&runner, sources[0]), 1);
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id != sources[1]));
}

const LAIRWATCH: &str = "This creature can block an additional creature each combat.\nWhenever this creature blocks two or more creatures, it gains first strike until end of turn.";
const RASHKA: &str = "Reach (This creature can block creatures with flying.)\nWhenever Rashka blocks one or more black creatures, Rashka gets +1/+2 until end of turn.";

fn count_qualified_setup(
    oracle: &str,
    name: &str,
    black: &[bool],
) -> (GameRunner, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = black
        .iter()
        .enumerate()
        .map(|(index, is_black)| {
            scenario
                .add_creature(P0, &format!("Attacker {index}"), 1, 1)
                .with_color(vec![if *is_black {
                    ManaColor::Black
                } else {
                    ManaColor::Green
                }])
                .id()
        })
        .collect();
    let source = scenario
        .add_creature(P1, name, 4, 4)
        .from_oracle_text_with_keywords(
            if name == "Rashka the Slayer" {
                &["Reach"]
            } else {
                &[]
            },
            oracle,
        )
        .with_color(vec![ManaColor::White])
        .id();
    if name == "Rashka the Slayer" {
        scenario.add_enchantment_from_oracle(P1, "High Ground", HIGH_GROUND);
    }
    let runner = scenario.build();
    let object = &runner.state().objects[&source];
    assert_eq!(object.controller, P1);
    assert_eq!(object.color, vec![ManaColor::White]);
    assert_eq!(object.trigger_definitions.len(), 1);
    assert_eq!(
        object.trigger_definitions[0].definition.mode,
        TriggerMode::Blocks
    );
    assert!(!serde_json::to_string(object)
        .unwrap()
        .contains("Unimplemented"));
    for (attacker, is_black) in attackers.iter().zip(black) {
        let object = &runner.state().objects[attacker];
        assert_eq!(object.controller, P0);
        assert_eq!(
            object.color,
            vec![if *is_black {
                ManaColor::Black
            } else {
                ManaColor::Green
            }]
        );
    }
    (runner, attackers, source)
}

fn actual_source_stack_count(runner: &GameRunner, source: ObjectId) -> usize {
    for entry in runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
    {
        assert_eq!(entry.controller, P1);
        assert!(matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
    }
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

#[test]
fn count_qualified_lairwatch_one_attacker_does_not_trigger() {
    let (mut runner, attackers, source) =
        count_qualified_setup(LAIRWATCH, "Lairwatch Giant", &[false]);
    declare(&mut runner, &attackers, &[source]);
    let observed = actual_source_stack_count(&runner, source);
    runner.advance_until_stack_empty();
    let first_strike = engine::game::keywords::has_keyword(
        &runner.state().objects[&source],
        &Keyword::FirstStrike,
    );
    eprintln!("MEASURED Lairwatch one attacker triggers={observed}, first_strike={first_strike}");
    // CR 509.3e: one blocked attacker does not meet the printed minimum of two.
    assert_eq!((observed, first_strike), (0, false));
    count_qualified_lairwatch_two_attackers_triggers_once();
}

#[test]
fn count_qualified_lairwatch_two_attackers_triggers_once() {
    let (mut runner, attackers, source) =
        count_qualified_setup(LAIRWATCH, "Lairwatch Giant", &[false, false]);
    declare(&mut runner, &attackers, &[source]);
    let observed = actual_source_stack_count(&runner, source);
    runner.advance_until_stack_empty();
    let first_strike = engine::game::keywords::has_keyword(
        &runner.state().objects[&source],
        &Keyword::FirstStrike,
    );
    eprintln!("MEASURED Lairwatch two attackers triggers={observed}, first_strike={first_strike}");
    // CR 509.3e + CR 603.2: meeting the minimum triggers once for this declaration.
    assert_eq!((observed, first_strike), (1, true));
}

fn rashka_count_qualified_case(black: &[bool], expected: usize) {
    let (mut runner, attackers, source) = count_qualified_setup(RASHKA, "Rashka the Slayer", black);
    declare(&mut runner, &attackers, &[source]);
    let observed = actual_source_stack_count(&runner, source);
    runner.advance_until_stack_empty();
    let object = &runner.state().objects[&source];
    eprintln!(
        "MEASURED Rashka black={black:?}, triggers={observed}, resolved P/T={:?}/{:?}",
        object.power, object.toughness
    );
    // CR 603.1 + CR 603.2: the printed trigger requires at least one black blocked creature.
    // CR 509.3e: the matching count is evaluated for this block declaration.
    assert_eq!(
        (observed, object.power, object.toughness),
        (
            expected,
            Some(4 + expected as i32),
            Some(4 + 2 * expected as i32)
        )
    );
}

#[test]
fn count_qualified_rashka_nonblack_does_not_trigger() {
    rashka_count_qualified_case(&[false], 0);
    rashka_count_qualified_case(&[true], 1);
}

#[test]
fn count_qualified_rashka_mixed_attackers_triggers_once() {
    rashka_count_qualified_case(&[false, true], 1);
}

#[test]
fn count_qualified_rashka_two_black_attackers_triggers_once() {
    rashka_count_qualified_case(&[true, true], 1);
}

fn rashka_same_name_sources_case(second_black: bool) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let black = scenario
        .add_creature(P0, "Black attacker", 1, 1)
        .with_color(vec![ManaColor::Black])
        .id();
    let green = scenario
        .add_creature(P0, "Second attacker", 1, 1)
        .with_color(vec![if second_black {
            ManaColor::Black
        } else {
            ManaColor::Green
        }])
        .id();
    let sources: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_creature(P1, "Rashka the Slayer", 4, 4)
                .from_oracle_text_with_keywords(&["Reach"], RASHKA)
                .with_color(vec![ManaColor::White])
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    assert_ne!(sources[0], sources[1]);
    for source in &sources {
        let object = &runner.state().objects[source];
        assert_eq!(object.controller, P1);
        assert_eq!(object.trigger_definitions.len(), 1);
        assert_eq!(
            object.trigger_definitions[0].definition.mode,
            TriggerMode::Blocks
        );
        assert!(!serde_json::to_string(object)
            .unwrap()
            .contains("Unimplemented"));
    }
    assert_eq!(runner.state().objects[&black].color, vec![ManaColor::Black]);
    assert_eq!(
        runner.state().objects[&green].color,
        vec![if second_black {
            ManaColor::Black
        } else {
            ManaColor::Green
        }]
    );
    priority_to(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (black, AttackTarget::Player(P1)),
                (green, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .unwrap();
    priority_to(&mut runner, true);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(sources[0], black), (sources[1], green)],
        })
        .unwrap();
    let combat = runner.state().combat.as_ref().unwrap();
    assert_eq!(combat.blocker_to_attacker[&sources[0]], vec![black]);
    assert_eq!(combat.blocker_to_attacker[&sources[1]], vec![green]);
    assert_eq!(combat.blocker_assignments[&black], vec![sources[0]]);
    assert_eq!(combat.blocker_assignments[&green], vec![sources[1]]);
    if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner.act(GameAction::OrderTriggers { order }).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let observed: Vec<_> = sources
        .iter()
        .map(|source| actual_source_stack_count(&runner, *source))
        .collect();
    runner.advance_until_stack_empty();
    let stats: Vec<_> = sources
        .iter()
        .map(|source| {
            let object = &runner.state().objects[source];
            (object.power, object.toughness)
        })
        .collect();
    eprintln!("MEASURED same-name Rashkas triggers={observed:?}, P/T={stats:?}");
    // CR 603.1 + CR 603.2: each source checks the creatures it actually blocks.
    assert_eq!(observed, vec![1, usize::from(second_black)]);
    assert_eq!(
        stats,
        vec![
            (Some(5), Some(6)),
            if second_black {
                (Some(5), Some(6))
            } else {
                (Some(4), Some(4))
            }
        ]
    );
}

#[test]
fn count_qualified_rashka_same_name_sources_match_their_own_groups() {
    rashka_same_name_sources_case(false);
    // Same-test positive: both independently qualifying identities fire once.
    rashka_same_name_sources_case(true);
}

fn public_count_qualified_removal_case(initial_attackers: usize, oracle: &str, name: &str) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..initial_attackers)
        .map(|index| {
            scenario
                .add_creature(P0, &format!("Removal attacker {index}"), 1, 1)
                .with_color(vec![if index == 0 {
                    ManaColor::Black
                } else {
                    ManaColor::Green
                }])
                .id()
        })
        .collect();
    let source = scenario
        .add_creature(P1, name, 4, 4)
        .from_oracle_text_with_keywords(
            if name == "Rashka the Slayer" {
                &["Reach"]
            } else {
                &[]
            },
            oracle,
        )
        .with_color(vec![ManaColor::White])
        .id();
    scenario.add_enchantment_from_oracle(P1, "High Ground", HIGH_GROUND);
    // Stage a zero-cost instant: this fixture measures removal, not payment.
    // Oracle verified from the same pinned AtomicCards as the combat sources.
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .id();
    let mut runner = scenario.build();
    for id in [source, spell] {
        assert!(!serde_json::to_string(&runner.state().objects[&id])
            .unwrap()
            .contains("Unimplemented"));
    }
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_eq!(runner.state().objects[&source].trigger_definitions.len(), 1);
    assert_eq!(
        runner.state().objects[&source].trigger_definitions[0]
            .definition
            .mode,
        TriggerMode::Blocks
    );
    declare(&mut runner, &attackers, &[source]);
    assert_eq!(actual_source_stack_count(&runner, source), 1);
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .unwrap();
    assert_eq!(entry.controller, P1);
    // CR 603.4: event-time count is consumed before the independent stack entry.
    assert!(matches!(
        entry.kind,
        StackEntryKind::TriggeredAbility {
            condition: None,
            ..
        }
    ));
    let mut cast = runner.cast(spell).target_object(attackers[0]).commit();
    assert_eq!(
        cast.state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count(),
        1
    );
    for _ in 0..16 {
        if !cast
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell)
        {
            break;
        }
        assert!(matches!(
            cast.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        cast.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(cast.state().objects[&attackers[0]].zone, Zone::Hand);
    assert!(!cast
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == spell));
    let remaining = &cast.state().combat.as_ref().unwrap().blocker_to_attacker[&source];
    assert_eq!(remaining.len(), initial_attackers - 1);
    assert!(!remaining.contains(&attackers[0]));
    let after_removal = cast
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count();
    assert!(!engine::game::keywords::has_keyword(
        &cast.state().objects[&source],
        &Keyword::FirstStrike
    ));
    if name == "Rashka the Slayer" {
        let object = &cast.state().objects[&source];
        assert_eq!((object.power, object.toughness), (Some(4), Some(4)));
    }
    eprintln!("MEASURED {name} {initial_attackers}→{} public Unsummon: unresolved source triggers={after_removal}", initial_attackers - 1);
    // CR 603.3: removal preserves the already-triggered stack object.
    // CR 509.3e: this removal does not newly meet the at-least-two threshold.
    assert_eq!(after_removal, 1);
    let outcome = cast.resolve();
    outcome.assert_zone(&[attackers[0]], Zone::Hand);
    assert!(outcome.state().stack.is_empty());
    // CR 603.1 + CR 603.3: the event qualification is not an intervening-if recheck.
    if name == "Rashka the Slayer" {
        let object = &outcome.state().objects[&source];
        assert_eq!((object.power, object.toughness), (Some(5), Some(6)));
    } else {
        assert!(engine::game::keywords::has_keyword(
            &outcome.state().objects[&source],
            &Keyword::FirstStrike
        ));
    }
}

#[test]
fn count_qualified_lairwatch_removal_two_to_one_preserves_trigger() {
    public_count_qualified_removal_case(2, LAIRWATCH, "Lairwatch Giant");
    count_qualified_lairwatch_two_attackers_triggers_once();
}

#[test]
fn count_qualified_lairwatch_removal_three_to_two_does_not_retrigger() {
    public_count_qualified_removal_case(3, LAIRWATCH, "Lairwatch Giant");
    count_qualified_lairwatch_two_attackers_triggers_once();
}

#[test]
fn count_qualified_rashka_removal_of_sole_black_preserves_trigger() {
    public_count_qualified_removal_case(2, RASHKA, "Rashka the Slayer");
    rashka_count_qualified_case(&[true, false], 1);
}

// CR 201.5 + CR 509.3e: only this source's blocked attackers count.
#[test]
fn count_qualified_lairwatch_unrelated_and_unassigned_attackers_do_not_count() {
    for source_assignments in [0, 1] {
        let (mut runner, attackers, source) =
            count_qualified_setup(LAIRWATCH, "Lairwatch Giant", &[false, false]);
        priority_to(&mut runner, false);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: attackers
                    .iter()
                    .map(|id| (*id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            })
            .unwrap();
        priority_to(&mut runner, true);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: attackers
                    .iter()
                    .take(source_assignments)
                    .map(|id| (source, *id))
                    .collect(),
            })
            .unwrap();
        let combat = runner.state().combat.as_ref().unwrap();
        assert_eq!(
            combat.blocker_to_attacker.get(&source).map_or(0, Vec::len),
            source_assignments
        );
        assert_eq!(actual_source_stack_count(&runner, source), 0);
        runner.advance_until_stack_empty();
        assert!(!engine::game::keywords::has_keyword(
            &runner.state().objects[&source],
            &Keyword::FirstStrike
        ));
    }
    count_qualified_lairwatch_two_attackers_triggers_once();
}

#[test]
fn count_qualified_three_attacker_threshold_runs_through_public_combat() {
    let oracle = "This creature can block an additional creature each combat.\nWhenever this creature blocks three or more creatures, it gains first strike until end of turn.";
    // Synthetic threshold parameter; printed Lairwatch remains verbatim elsewhere.
    for (number, expected) in [(2, 0), (3, 1)] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let attackers: Vec<_> = (0..number)
            .map(|i| {
                scenario
                    .add_creature(P0, &format!("Threshold attacker {i}"), 1, 1)
                    .id()
            })
            .collect();
        let source = scenario
            .add_creature_from_oracle(P1, "Threshold Guard", 4, 4, oracle)
            .id();
        scenario.add_enchantment_from_oracle(P1, "High Ground", HIGH_GROUND);
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&source].trigger_definitions.len(), 1);
        assert!(!serde_json::to_string(&runner.state().objects[&source])
            .unwrap()
            .contains("Unimplemented"));
        declare(&mut runner, &attackers, &[source]);
        assert_eq!(actual_source_stack_count(&runner, source), expected);
        runner.advance_until_stack_empty();
        assert_eq!(
            engine::game::keywords::has_keyword(
                &runner.state().objects[&source],
                &Keyword::FirstStrike
            ),
            expected == 1
        );
    }
}

// CR 201.5 + CR 509.3e: another blocker's group cannot satisfy this source.
#[test]
fn count_qualified_lairwatch_empty_source_ignores_other_blocker_group() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..2)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Other group attacker {i}"), 1, 1)
                .id()
        })
        .collect();
    let source = scenario
        .add_creature_from_oracle(P1, "Lairwatch Giant", 4, 4, LAIRWATCH)
        .with_color(vec![engine::types::mana::ManaColor::White])
        .id();
    let other = scenario
        .add_creature_from_oracle(P1, "Lairwatch Giant", 4, 4, LAIRWATCH)
        .with_color(vec![engine::types::mana::ManaColor::White])
        .id();
    let mut runner = scenario.build();
    for id in [source, other] {
        assert_eq!(runner.state().objects[&id].trigger_definitions.len(), 1);
        assert!(!serde_json::to_string(&runner.state().objects[&id])
            .unwrap()
            .contains("Unimplemented"));
    }
    declare(&mut runner, &attackers, &[other]);
    assert!(!runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .blocker_to_attacker
        .contains_key(&source));
    assert_eq!(actual_source_stack_count(&runner, source), 0);
    assert_eq!(actual_source_stack_count(&runner, other), 1);
    runner.advance_until_stack_empty();
    assert!(!engine::game::keywords::has_keyword(
        &runner.state().objects[&source],
        &Keyword::FirstStrike
    ));
    assert!(engine::game::keywords::has_keyword(
        &runner.state().objects[&other],
        &Keyword::FirstStrike
    ));
}

// CR 509.3e + CR 509.3f: an unassigned black attacker cannot satisfy Rashka.
#[test]
fn count_qualified_rashka_unassigned_black_attacker_does_not_count() {
    let (mut runner, attackers, source) =
        count_qualified_setup(RASHKA, "Rashka the Slayer", &[false, true]);
    priority_to(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .unwrap();
    priority_to(&mut runner, true);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(source, attackers[0])],
        })
        .unwrap();
    assert_eq!(
        runner.state().combat.as_ref().unwrap().blocker_to_attacker[&source],
        vec![attackers[0]]
    );
    assert_eq!(actual_source_stack_count(&runner, source), 0);
    runner.advance_until_stack_empty();
    let object = &runner.state().objects[&source];
    assert_eq!((object.power, object.toughness), (Some(4), Some(4)));
    rashka_count_qualified_case(&[false, true], 1);
}

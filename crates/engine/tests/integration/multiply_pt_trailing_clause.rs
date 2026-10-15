//! A same-sentence tail after a P/T multiply ("double its power and toughness and
//! this Saga gains hexproof") must never be dropped while the clause is reported
//! as supported (issue #9777). CR 608.2c: the controller follows all of an
//! ability's instructions in the order written. CR 701.10a: doubling P/T is a
//! continuous effect.
//!
//! No printed card carries an unparsed tail after a P/T multiply. World War Hulk
//! III ("Until end of turn, double its power and toughness and it gains trample.")
//! is the closest printed card and the green control: its "it gains" conjunct is
//! split off before the multiply parser sees the clause. The synthetic chapters
//! swap that conjunct for a grant to the Saga itself.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::turns::execute_cleanup;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, ContinuousModification, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const SAGA_REMINDER: &str =
    "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)";
const HULK_I_II: &str = "I \u{2014} The next red or green creature spell you cast this turn can be cast without paying its mana cost.\nII \u{2014} Put three +1/+1 counters on target creature you control.";
const HULK_III: &str = "Choose target creature you control. Until end of turn, double its power and toughness and it gains trample.";
const SELF_GRANT_III: &str = "Choose target creature you control. Until end of turn, double its power and toughness and this Saga gains hexproof.";

fn hulk_oracle(chapter_three: &str) -> String {
    format!("{SAGA_REMINDER}\n{HULK_I_II}\nIII \u{2014} {chapter_three}")
}

/// Every effect node in an ability chain: sub, else, and mode branches.
fn collect(def: &AbilityDefinition, out: &mut Vec<Effect>) {
    out.push((*def.effect).clone());
    if let Some(sub) = def.sub_ability.as_deref() {
        collect(sub, out);
    }
    if let Some(other) = def.else_ability.as_deref() {
        collect(other, out);
    }
    for mode in &def.mode_abilities {
        collect(mode, out);
    }
}

fn hulk_chapter_three(name: &str, chapter_three: &str) -> Vec<Effect> {
    let oracle = hulk_oracle(chapter_three);
    let parsed = parse_oracle_text(
        &oracle,
        name,
        &[],
        &["Enchantment".to_string()],
        &["Saga".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.saga_chapter == Some(3))
        .unwrap_or_else(|| panic!("reach: {name} has a chapter III trigger: {parsed:#?}"));
    let mut out = Vec::new();
    collect(
        trigger
            .execute
            .as_deref()
            .expect("chapter III has an ability"),
        &mut out,
    );
    out
}

/// Every effect node a creature card's abilities and triggers produce.
fn creature_effects(name: &str, oracle: &str) -> Vec<Effect> {
    let parsed = parse_oracle_text(
        oracle,
        name,
        &[],
        &["Creature".to_string()],
        &["Ape".to_string()],
    );
    let mut out = Vec::new();
    for ability in &parsed.abilities {
        collect(ability, &mut out);
    }
    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            collect(execute, &mut out);
        }
    }
    out
}

fn is_multiply_pt(effect: &Effect) -> bool {
    matches!(effect, Effect::DoublePT { .. } | Effect::DoublePTAll { .. })
}

/// The explicit gap the multiply-P/T parser emits for an unparsed tail, carrying
/// the clause it refused.
fn is_trailing_gap(effect: &Effect, fragment: &str) -> bool {
    matches!(effect, Effect::Unimplemented { name, description: Some(d) }
        if name == "multiply_pt_trailing_clause" && d.contains(fragment))
}

/// Control. Printed World War Hulk III keeps its DoublePT and its trample grant
/// to the chosen creature.
#[test]
fn world_war_hulk_keeps_double_pt_and_its_gains_conjunct() {
    let nodes = hulk_chapter_three("World War Hulk", HULK_III);
    assert!(
        nodes.iter().any(|e| matches!(
            e,
            Effect::DoublePT {
                target: TargetFilter::ParentTarget,
                factor: 2,
                ..
            }
        )),
        "chapter III doubles the chosen creature's P/T, got {nodes:#?}"
    );
    let trample_grants: Vec<_> = nodes
        .iter()
        .filter_map(|e| match e {
            Effect::GenericEffect {
                static_abilities, ..
            } => Some(static_abilities),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        trample_grants.len() == 1
            && trample_grants[0].affected == Some(TargetFilter::ParentTarget)
            && trample_grants[0].modifications
                == vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Trample,
                }],
        "\"and it gains trample\" grants exactly trample to the chosen creature, \
         got {trample_grants:#?}"
    );
    assert!(
        nodes
            .iter()
            .all(|e| e.unimplemented_description().is_none()),
        "the printed chapter has no gap, got {nodes:#?}"
    );
}

/// Defect. A grant to the Saga in the same sentence as the doubling fails the
/// clause closed instead of vanishing behind a DoublePT.
#[test]
fn saga_self_grant_after_double_pt_fails_closed() {
    for tail in [
        "this Saga gains hexproof.",
        "this Saga gains \"{T}: Add {C}.\"",
    ] {
        let nodes = hulk_chapter_three(
            "Synthetic Hulk Saga",
            &format!(
                "Choose target creature you control. Until end of turn, double its power and toughness and {tail}"
            ),
        );
        assert!(
            matches!(nodes.first(), Some(Effect::TargetOnly { .. })),
            "reach: chapter III with tail {tail:?} parses its leading choice, got {nodes:#?}"
        );
        assert!(
            nodes
                .iter()
                .any(|e| is_trailing_gap(e, "double its power and toughness and ~ gains")),
            "the tail {tail:?} must fail the doubling clause closed, got {nodes:#?}"
        );
        assert!(
            !nodes.iter().any(is_multiply_pt),
            "a DoublePT must not claim a clause whose tail it dropped, got {nodes:#?}"
        );
    }
}

/// Class. Every multiply-P/T arm and both multiplier words fail closed on an
/// unparsed same-sentence tail outside a Saga, while the printed form of each arm
/// (tail-free) still parses with no gap.
#[test]
fn every_multiply_pt_arm_fails_closed_on_a_trailing_tail() {
    let arms = [
        // possessive target (Bulk Up)
        "{1}: Double target creature's power",
        // self possessive (Targ Nar)
        "{1}: Double Synthetic Ape's power and toughness",
        // the … of target (Unleash Fury)
        "{1}: Double the power of target creature",
        // the … of each (Roar of Endless Song III)
        "{1}: Double the power and toughness of each creature you control",
        // anaphoric its (Grunn)
        "Whenever Synthetic Ape attacks, double its power and toughness",
        // triple (Tifa's Limit Break, Final Heaven)
        "{1}: Triple target creature's power and toughness",
    ];
    for head in arms {
        let printed = format!("{head} until end of turn.");
        let nodes = creature_effects("Synthetic Ape", &printed);
        assert!(
            nodes.iter().any(is_multiply_pt),
            "reach: {printed:?} parses as a P/T multiply, got {nodes:#?}"
        );
        assert!(
            nodes
                .iter()
                .all(|e| e.unimplemented_description().is_none()),
            "{printed:?} has no gap, got {nodes:#?}"
        );

        let tailed = format!("{head} and Synthetic Ape gains hexproof until end of turn.");
        let nodes = creature_effects("Synthetic Ape", &tailed);
        assert!(
            nodes
                .iter()
                .any(|e| is_trailing_gap(e, "and ~ gains hexproof")),
            "{tailed:?} must fail closed, got {nodes:#?}"
        );
        assert!(
            !nodes.iter().any(is_multiply_pt),
            "{tailed:?} must not report a P/T multiply, got {nodes:#?}"
        );
    }
}

fn lore_count(runner: &GameRunner, saga: ObjectId) -> u32 {
    runner.state().objects[&saga]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

/// A Saga built from `oracle` with two lore counters, and a vanilla 2/2 P0
/// controls, so the lore counter added after P0's draw step fires chapter III.
fn hulk_board(name: &str, oracle: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, name, 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(oracle)
        .id();
    scenario.with_counter(saga, CounterType::Lore, 2);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    (scenario.build(), saga, bears)
}

/// CR 714.3c + CR 714.2b: advance from P0's upkeep through the draw step, where
/// the third lore counter fires chapter III. The 2/2 is the only creature P0
/// controls, so it is the chapter's target; resolve the stack through the
/// runner. Returns whether the chapter trigger was on the stack (the reach
/// guard).
fn fire_chapter_three(runner: &mut GameRunner, saga: ObjectId) -> bool {
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        lore_count(runner, saga),
        3,
        "CR 714.3c must add the third lore counter, firing chapter III"
    );

    let mut on_stack = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the creature");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                on_stack |= runner
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.source_id == saga);
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving chapter III: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "chapter III must resolve, waiting_for={:?}",
        runner.state().waiting_for
    );
    on_stack
}

/// Runtime control. World War Hulk III makes the chosen 2/2 a 4/4 with trample
/// until end of turn (CR 701.10a + CR 514.2), with no unsupported telemetry.
#[test]
fn world_war_hulk_chapter_three_doubles_and_grants_trample_until_end_of_turn() {
    let (mut runner, saga, bears) = hulk_board("World War Hulk", &hulk_oracle(HULK_III));

    assert!(
        fire_chapter_three(&mut runner, saga),
        "reach: chapter III went on the stack"
    );
    let obj = &runner.state().objects[&bears];
    assert_eq!((obj.power, obj.toughness), (Some(4), Some(4)), "doubled");
    assert!(obj.has_keyword(&Keyword::Trample), "gained trample");
    assert!(
        runner.state().unimplemented_oracle_ids.is_empty(),
        "the printed chapter hits no unsupported effect, got {:?}",
        runner.state().unimplemented_oracle_ids
    );

    // CR 514.2: both effects end in the cleanup step.
    let mut events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut events);
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&bears];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(2)),
        "the doubling ends at cleanup"
    );
    assert!(
        !obj.has_keyword(&Keyword::Trample),
        "trample ends at cleanup"
    );
}

/// Runtime defect. With the trample conjunct replaced by a grant to the Saga,
/// the Saga in play carries chapter III's clause as an explicit gap, and
/// resolving the chapter does not apply the doubling on its own.
///
/// The resolver skips an `Unimplemented` node inside a chain before the
/// `unimplemented_oracle_ids` recorder, so that set is not a signal here; the
/// gap is read from the live object's chapter definition instead.
#[test]
fn saga_self_grant_chapter_three_fails_closed_without_partial_doubling() {
    let (mut runner, saga, bears) = hulk_board("Synthetic Hulk Saga", &hulk_oracle(SELF_GRANT_III));

    let chapter_three = runner.state().objects[&saga]
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .find(|t| t.saga_chapter == Some(3))
        .cloned()
        .expect("reach: the Saga in play has a chapter III trigger");
    let mut nodes = Vec::new();
    collect(
        chapter_three
            .execute
            .as_deref()
            .expect("chapter III has an ability"),
        &mut nodes,
    );
    assert!(
        nodes
            .iter()
            .any(|e| is_trailing_gap(e, "double its power and toughness and ~ gains")),
        "the Saga in play carries the clause as an explicit gap, got {nodes:#?}"
    );

    assert!(
        fire_chapter_three(&mut runner, saga),
        "reach: chapter III went on the stack"
    );
    let obj = &runner.state().objects[&bears];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(2)),
        "no partial doubling: the clause is unsupported as a whole"
    );
}

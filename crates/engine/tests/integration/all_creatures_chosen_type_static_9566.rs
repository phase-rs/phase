//! Engineered Plague {2}{B} Enchantment — verbatim Oracle text (pinned MTGJSON):
//! "As this enchantment enters, choose a creature type.
//! All creatures of the chosen type get -1/-1."
//!
//! phase-rs/phase#9566: the "All creatures [qualifier] get ..." static dispatch arm
//! dropped the "of the chosen type" qualifier, so the -1/-1 hit every creature.
//!
//! - CR 614.1c + CR 614.12: "As this enchantment enters, choose ..." is a
//!   replacement effect that modifies how the permanent enters.
//! - CR 607.2d: the "chosen type" in the static ability is linked to that choice.
//! - CR 611.3a: the static's continuous effect applies at any given moment to
//!   whatever its text indicates (re-evaluated live, not locked in).
//! - CR 611.3c: it applies to a permanent as that permanent enters.
//! - CR 613.4c: -1/-1 is a layer 7c modification.
//! - CR 704.5f: a creature with toughness 0 or less is put into its graveyard.
//! - CR 702.73a: a Changeling is every creature type.
//!
//! Every negative assertion (a creature outside the chosen type is untouched) is
//! paired with a positive reach-guard in the same test (a creature of the chosen
//! type shrank), so the negatives cannot pass vacuously.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::{NamedChoiceSourceBinding, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ENGINEERED_PLAGUE: &str =
    "As this enchantment enters, choose a creature type.\nAll creatures of the chosen type get -1/-1.";

fn plague_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Engineered Plague", false)
        .as_enchantment()
        .from_oracle_text(ENGINEERED_PLAGUE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black],
        })
        .id()
}

fn creature_in_hand(scenario: &mut GameScenario, name: &str, subtype: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 2, 2, "")
        .with_subtypes(vec![subtype])
        .with_mana_cost(ManaCost::generic(1))
        .id()
}

/// Every creature type the boards use; `GameScenario` leaves the list empty and
/// the as-enters chooser offers it.
fn seed_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = [
        "Elemental",
        "Human",
        "Wizard",
        "Lhurgoyf",
        "Goblin",
        "Shapeshifter",
    ]
    .iter()
    .map(|t| t.to_string())
    .collect();
}

fn fund(runner: &mut GameRunner, source: ObjectId, units: usize) {
    for _ in 0..units {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(ManaType::Black, source, false, vec![]));
    }
}

/// Cast an Engineered Plague through the real pipeline and answer its as-enters
/// creature-type choice with `chosen`. The prompt is asserted unconditionally.
fn cast_plague_naming(runner: &mut GameRunner, plague: ObjectId, chosen: &str) {
    fund(runner, plague, 3);
    runner.cast(plague).resolve();

    // CR 614.12: the choice is raised while the Plague is still entering.
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        source,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Engineered Plague must pause on the as-enters creature type choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        matches!(choice_type, ChoiceType::CreatureType { .. }),
        "the choice is a creature type, got {choice_type:?}"
    );
    assert!(
        options.iter().any(|option| option == chosen),
        "{chosen} must be offered, options={options:?}"
    );
    assert_eq!(
        source.map(|source| source.binding),
        Some(NamedChoiceSourceBinding::PendingEntry),
        "the answer belongs to the parked battlefield entry"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: chosen.to_string(),
        })
        .expect("choose creature type");
    runner.advance_until_stack_empty();

    assert!(runner.state().stack.is_empty(), "completion: empty stack");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: priority returns, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&plague].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&plague].chosen_creature_type(),
        Some(chosen),
        "the Plague remembers the chosen type (CR 607.2d)"
    );
}

fn pt(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = &runner.state().objects[&id];
    (obj.power, obj.toughness)
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// The issue repro: naming Elemental shrinks only Elementals, whoever controls
/// them; every other creature is untouched.
#[test]
fn naming_elemental_shrinks_only_elementals_on_both_sides() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Textless Human Wizard filler: Engineered Plague must not affect it when
    // Elemental is chosen (Oracle text above; CR 611.3a).
    let opposing_wizard = scenario
        .add_creature(P1, "Opposing Human Wizard", 1, 1)
        .with_subtypes(vec!["Human", "Wizard"])
        .id();
    let opposing_lhurgoyf = scenario
        .add_creature(P1, "Opposing Lhurgoyf", 4, 4)
        .with_subtypes(vec!["Lhurgoyf"])
        .id();
    let opposing_elemental = scenario
        .add_creature(P1, "Opposing Elemental", 1, 1)
        .with_subtypes(vec!["Elemental"])
        .id();
    let own_elemental = scenario
        .add_creature(P0, "Own Elemental", 3, 3)
        .with_subtypes(vec!["Elemental"])
        .id();
    // Woodland Changeling (MTGJSON): "Changeling (This card is every creature type.)"
    // CR 702.73a: Woodland Changeling is an Elemental as well as every other
    // creature type, so Engineered Plague's chosen-Elemental effect applies.
    let changeling = scenario
        .add_creature(P1, "Woodland Changeling", 2, 2)
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .from_oracle_text_with_keywords(
            &["Changeling"],
            "Changeling (This card is every creature type.)",
        )
        .id();
    let plague = plague_in_hand(&mut scenario);

    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    cast_plague_naming(&mut runner, plague, "Elemental");

    // Positive reach-guards: the effect applied to Elementals on both sides, to a
    // Changeling (CR 702.73a), and killed a 1/1 Elemental (CR 704.5f).
    assert_eq!(
        zone(&runner, opposing_elemental),
        Zone::Graveyard,
        "CR 704.5f: the opposing 1/1 Elemental is put into its graveyard"
    );
    assert_eq!(
        pt(&runner, own_elemental),
        (Some(2), Some(2)),
        "\"All creatures\" is controller-agnostic: the caster's own Elemental shrinks"
    );
    assert_eq!(
        pt(&runner, changeling),
        (Some(1), Some(1)),
        "CR 702.73a: a Changeling is an Elemental"
    );

    // Negatives: creatures that are not Elementals are untouched.
    assert_eq!(zone(&runner, opposing_wizard), Zone::Battlefield);
    assert_eq!(pt(&runner, opposing_wizard), (Some(1), Some(1)));
    assert_eq!(zone(&runner, opposing_lhurgoyf), Zone::Battlefield);
    assert_eq!(pt(&runner, opposing_lhurgoyf), (Some(4), Some(4)));
}

/// Two Plagues naming different types on one board: each static reads its own
/// source's chosen type.
#[test]
fn each_plague_shrinks_only_its_own_chosen_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let elemental = scenario
        .add_creature(P1, "Elemental", 3, 3)
        .with_subtypes(vec!["Elemental"])
        .id();
    let goblin = scenario
        .add_creature(P1, "Goblin", 3, 3)
        .with_subtypes(vec!["Goblin"])
        .id();
    let human = scenario
        .add_creature(P0, "Human", 2, 2)
        .with_subtypes(vec!["Human"])
        .id();
    let plague_a = plague_in_hand(&mut scenario);
    let plague_b = plague_in_hand(&mut scenario);

    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    cast_plague_naming(&mut runner, plague_a, "Elemental");
    assert_eq!(
        pt(&runner, elemental),
        (Some(2), Some(2)),
        "Plague A: Elemental"
    );
    assert_eq!(
        pt(&runner, goblin),
        (Some(3), Some(3)),
        "Plague A spares Goblin"
    );

    cast_plague_naming(&mut runner, plague_b, "Goblin");
    // Positive: the second Plague shrank the Goblin by exactly one.
    assert_eq!(pt(&runner, goblin), (Some(2), Some(2)), "Plague B: Goblin");
    // Negatives: the Elemental is shrunk only by Plague A (not twice), and the
    // Human by neither.
    assert_eq!(
        pt(&runner, elemental),
        (Some(2), Some(2)),
        "Plague B spares Elemental"
    );
    assert_eq!(zone(&runner, human), Zone::Battlefield);
    assert_eq!(pt(&runner, human), (Some(2), Some(2)));
}

/// CR 611.3a + CR 611.3c: the effect is live, so creatures entering after the
/// Plague are shrunk only if they are of the chosen type.
#[test]
fn elemental_entering_later_is_shrunk_and_others_are_not() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let plague = plague_in_hand(&mut scenario);
    let late_elemental = creature_in_hand(&mut scenario, "Late Elemental", "Elemental");
    let late_goblin = creature_in_hand(&mut scenario, "Late Goblin", "Goblin");

    let mut runner = scenario.build();
    seed_creature_types(&mut runner);
    cast_plague_naming(&mut runner, plague, "Elemental");

    for creature in [late_elemental, late_goblin] {
        fund(&mut runner, creature, 1);
        runner.cast(creature).resolve();
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty(), "completion: empty stack");
        assert_eq!(zone(&runner, creature), Zone::Battlefield);
    }

    // Positive reach-guard: the late Elemental shrank.
    assert_eq!(pt(&runner, late_elemental), (Some(1), Some(1)));
    // Negative: the late Goblin is untouched.
    assert_eq!(pt(&runner, late_goblin), (Some(2), Some(2)));
}

//! A3.8 (C3.7): the outer resolution's population is isolated across a
//! nested post-replacement chain.
//!
//! `REVEAL_MUSTER` (the synthetic building-block instrument of
//! `for_each_of_those_population`) reveals True-Name Nemesis LAST among its
//! two kept creatures. Nemesis's "As this creature enters, choose a player"
//! (CR 614.1c, CR 614.12a) pauses the reveal's delivery inside a nested
//! post-replacement chain; after the answer the outer resolution resumes.
//!
//! Derived reading (CR 701.20a, CR 608.2c, CR 614.12a, CR 611.2c): every kept
//! creature, Nemesis included, enters; the outer's later instructions ("They
//! gain haste until end of turn", "For each of those creatures, put a +1/+1
//! counter on that creature") name exactly the kept set; Nemesis's chosen
//! player is its own.
//!
//! LABELLED REGRESSION GUARD: at this phase's base the measurement (probe
//! P3c.1) found no isolation defect — every kept creature entered, the kept
//! set was published, and the iteration covered exactly it — so the nested
//! chain needed no channel isolation and none was built. These rows claim no
//! discrimination; they keep the measured behaviour from regressing.
//!
//! Reveal-order constraint: Nemesis is the last kept creature in every
//! library here.

use crate::for_each_of_those_population::{
    finish_muster_library, has_haste, muster_board, p1p1, stage_muster_library, total_p1p1,
    zone_of, MusterCard, REVEAL_MUSTER,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetFilter;
use engine::types::ability::{ChoiceType, ChosenAttribute, ContinuousModification};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const TRUE_NAME_NEMESIS: &str =
    "As this creature enters, choose a player.\nThis creature has protection from the chosen player.";

/// Top to bottom: a miss, Creature A, a miss, True-Name Nemesis (the last
/// kept creature), Creature C (beyond the count of two), a miss.
const NEMESIS_LAST: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::Creature("Creature A"),
    MusterCard::Land("Miss Two"),
    MusterCard::OracleCreature {
        name: "True-Name Nemesis",
        oracle: TRUE_NAME_NEMESIS,
        keywords: &[],
        power: 3,
        toughness: 1,
    },
    MusterCard::Creature("Creature C"),
    MusterCard::Sorcery("Miss Three"),
];

fn nemesis_prompt_bound(runner: &GameRunner, nemesis: ObjectId) -> bool {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::Player { .. },
            source,
            ..
        } => {
            source
                .as_ref()
                .map(|source| source.prompt.identity.reference.object_id)
                == Some(nemesis)
        }
        _ => false,
    }
}

/// The haste grants installed on exactly `id`.
fn haste_grants(runner: &GameRunner, id: ObjectId) -> usize {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.affected == TargetFilter::SpecificObject { id }
                && tce
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    })
        })
        .count()
}

/// Casts the instrument, answers Nemesis's as-enters prompt with P2, and
/// asserts the outer's later instructions read exactly the kept set
/// `{A, Nemesis}`. Returns `[A, Nemesis]`.
fn resolve_nemesis_last(
    runner: &mut GameRunner,
    spell: ObjectId,
    library: &[(&'static str, ObjectId)],
) -> [ObjectId; 2] {
    let card = |name: &str| {
        library
            .iter()
            .find(|(card, _)| *card == name)
            .map(|(_, id)| *id)
            .expect("staged card")
    };
    let a = card("Creature A");
    let nemesis = card("True-Name Nemesis");
    let c = card("Creature C");
    // Board reach-guard: Nemesis carries its Moved replacement.
    assert!(runner.state().objects[&nemesis]
        .replacement_definitions
        .as_slice()
        .iter()
        .any(|r| r.event == ReplacementEvent::Moved && r.execute.is_some()));

    runner.cast(spell).resolve();
    assert!(
        nemesis_prompt_bound(runner, nemesis),
        "reach-guard: the nested chain paused on Nemesis's own prompt: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseOption {
            choice: P2.0.to_string(),
        })
        .expect("answer Nemesis");
    for _ in 0..6 {
        let state = runner.state();
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
            break;
        }
        runner.resolve_top();
    }

    let state = runner.state();
    // Reach-guard (P3c.1's marker): every kept creature entered and Nemesis
    // kept its own choice (CR 614.12a).
    for kept in [a, nemesis] {
        assert_eq!(zone_of(runner, kept), Zone::Battlefield);
    }
    assert!(state.objects[&nemesis]
        .chosen_attributes
        .contains(&ChosenAttribute::Player(P2)));
    // CR 608.2c: the outer's later instructions name exactly the kept set.
    for kept in [a, nemesis] {
        assert!(has_haste(runner, kept), "{kept:?} gained haste");
        assert_eq!(p1p1(runner, kept), 1, "{kept:?} got exactly one counter");
        assert_eq!(haste_grants(runner, kept), 1);
    }
    assert_eq!(zone_of(runner, c), Zone::Library);
    assert_eq!(p1p1(runner, c), 0);
    assert!(state.resolution_stack.is_empty());
    [a, nemesis]
}

/// A3.8 (C3.7, labelled regression guard): with Nemesis revealed last, the
/// outer resolution's haste grant and per-member counters land on exactly the
/// kept set after the nested chain's answer, and nothing else is countered.
#[test]
fn outer_population_survives_the_nested_replacement_chain() {
    let mut board = muster_board(REVEAL_MUSTER, NEMESIS_LAST);
    let spell = board.spell;
    let library = board.library.clone();
    let kept = resolve_nemesis_last(&mut board.runner, spell, &library);
    assert_eq!(total_p1p1(&board.runner), 2);
    for bystander in [board.bystander_p0, board.bystander_p1] {
        assert!(!has_haste(&board.runner, bystander));
        assert_eq!(p1p1(&board.runner, bystander), 0);
    }
    for kept in kept {
        assert_eq!(board.runner.state().objects[&kept].controller, P0);
    }
}

/// A3.8 fresh-state sibling (labelled regression guard): a second instrument
/// cast after the first completes starts from its own population — its haste
/// grant and counters land only on the two creature cards it reveals, and the
/// first resolution's kept creatures keep exactly one counter and one grant.
#[test]
fn a_later_resolution_reads_only_its_own_population() {
    let mut scenario = GameScenario::new_n_player(4, 2026);
    scenario.at_phase(Phase::PreCombatMain);
    let first_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reveal Muster", false, REVEAL_MUSTER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let second_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reveal Muster Again", false, REVEAL_MUSTER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let new_one = scenario.add_creature_to_graveyard(P0, "New One", 2, 2).id();
    let new_two = scenario.add_creature_to_graveyard(P0, "New Two", 2, 2).id();
    let library = stage_muster_library(&mut scenario, P0, NEMESIS_LAST);
    scenario.add_creature(P1, "Bystander One", 1, 1);
    let mut runner = scenario.build();
    finish_muster_library(&mut runner, P0, NEMESIS_LAST, &library);

    let kept = resolve_nemesis_last(&mut runner, first_spell, &library);

    // Relocate the two new creature cards to the top of the library, as
    // `finish_muster_library` relocates an Oracle creature.
    for (index, id) in [new_one, new_two].into_iter().enumerate() {
        let state = runner.state_mut();
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .expect("P0 exists");
        player.graveyard.retain(|card| *card != id);
        player.library.insert(index, id);
        state.objects.get_mut(&id).expect("new card").zone = Zone::Library;
    }

    runner.cast(second_spell).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    for fresh in [new_one, new_two] {
        assert_eq!(zone_of(&runner, fresh), Zone::Battlefield, "reach-guard");
        assert_eq!(p1p1(&runner, fresh), 1, "the second cast's member");
        assert_eq!(haste_grants(&runner, fresh), 1);
    }
    for earlier in kept {
        assert_eq!(p1p1(&runner, earlier), 1, "the first cast's member");
        assert_eq!(haste_grants(&runner, earlier), 1);
    }
    assert_eq!(total_p1p1(&runner), 4);
}

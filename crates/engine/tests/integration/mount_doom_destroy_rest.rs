//! Mount Doom's final ability — resolution-time survivor choice followed by a
//! battlefield wipe of the remaining creatures.
//!
//! CR 608.2c/d: the controller chooses the spared creatures while resolving.
//! CR 701.8a: every other creature is destroyed and moves to its graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DESTROY_REST_ORACLE: &str = "Choose up to two creatures, then destroy the rest.";

/// Verbatim Duneblast Oracle text: the single-pick ("up to one") member of the
/// destroy-the-rest class.
const DUNEBLAST: &str = "Choose up to one creature. Destroy the rest.";

struct MountDoomPrompt {
    runner: GameRunner,
    creatures: [ObjectId; 4],
    noncreature: ObjectId,
}

fn mount_doom_prompt() -> MountDoomPrompt {
    destroy_rest_prompt(DESTROY_REST_ORACLE, "Mount Doom Test", (0, Some(2)))
}

/// Cast a destroy-the-rest spell with `oracle` over a board of four creatures and
/// an artifact, and resolve it up to its survivor choice, which must publish the
/// `expected` `(min, max)` over the four creatures.
fn destroy_rest_prompt(oracle: &str, name: &str, expected: (u32, Option<u32>)) -> MountDoomPrompt {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spared_own = scenario.add_creature(P0, "Spared Own", 2, 2).id();
    let destroyed_own = scenario.add_creature(P0, "Destroyed Own", 2, 2).id();
    let spared_opponent = scenario.add_creature(P1, "Spared Opponent", 2, 2).id();
    let destroyed_opponent = scenario.add_creature(P1, "Destroyed Opponent", 2, 2).id();
    let noncreature = scenario
        .add_artifact_from_oracle(P0, "Unaffected Artifact", "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the destroy-rest spell");
    runner.advance_until_stack_empty();

    let WaitingFor::ChooseObjectsSelection {
        min, max, eligible, ..
    } = &runner.state().waiting_for
    else {
        panic!("resolution must pause for the untargeted survivor choice");
    };
    assert_eq!((*min, *max), expected);
    assert_eq!(eligible.len(), 4);

    MountDoomPrompt {
        runner,
        creatures: [
            spared_own,
            destroyed_own,
            spared_opponent,
            destroyed_opponent,
        ],
        noncreature,
    }
}

#[test]
fn mount_doom_rejects_three_then_accepts_two_distinct_survivors() {
    let MountDoomPrompt {
        mut runner,
        creatures: [spared_own, destroyed_own, spared_opponent, destroyed_opponent],
        noncreature,
    } = mount_doom_prompt();

    let over_max = runner.act(GameAction::SelectTargets {
        targets: vec![
            TargetRef::Object(spared_own),
            TargetRef::Object(spared_opponent),
            TargetRef::Object(destroyed_own),
        ],
    });
    assert!(
        over_max.is_err(),
        "three choices must be rejected when max is two"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseObjectsSelection { .. }
    ));

    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(spared_own),
                TargetRef::Object(spared_opponent),
            ],
        })
        .expect("choose the two creatures to spare");
    runner.advance_until_stack_empty();

    for id in [spared_own, spared_opponent, noncreature] {
        assert_eq!(
            runner.state().objects.get(&id).map(|object| object.zone),
            Some(Zone::Battlefield),
            "chosen creatures and noncreature permanents must survive"
        );
    }
    for id in [destroyed_own, destroyed_opponent] {
        assert_eq!(
            runner.state().objects.get(&id).map(|object| object.zone),
            Some(Zone::Graveyard),
            "each unchosen creature must be destroyed"
        );
    }
}

#[test]
fn mount_doom_rejects_duplicate_then_accepts_zero_survivors() {
    let MountDoomPrompt {
        mut runner,
        creatures,
        noncreature,
    } = mount_doom_prompt();

    let duplicate = runner.act(GameAction::SelectTargets {
        targets: vec![
            TargetRef::Object(creatures[0]),
            TargetRef::Object(creatures[0]),
        ],
    });
    assert!(
        duplicate.is_err(),
        "the same object cannot fill two choice slots"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseObjectsSelection { .. }
    ));

    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("zero survivors is legal for an up-to-two choice");
    runner.advance_until_stack_empty();

    for id in creatures {
        assert_eq!(
            runner.state().objects.get(&id).map(|object| object.zone),
            Some(Zone::Graveyard),
            "choosing zero must destroy every creature"
        );
    }
    assert_eq!(
        runner
            .state()
            .objects
            .get(&noncreature)
            .map(|object| object.zone),
        Some(Zone::Battlefield)
    );
}

#[test]
fn choose_objects_runtime_rejects_fewer_than_required_minimum() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let required_choice = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::ChooseObjectsIntoTrackedSet {
            chooser: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::creature()),
            min: 1,
            max: Some(2),
            cardinality: None,
            eligibility: None,
        },
    );
    let host = {
        let mut builder = scenario.add_creature(P0, "Required Choice Host", 1, 1);
        builder.with_ability_definition(required_choice);
        builder.id()
    };
    scenario.add_creature(P0, "Eligible Creature", 1, 1);

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate required-choice ability");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseObjectsSelection {
            min: 1,
            max: Some(2),
            ..
        }
    ));
    assert!(
        runner
            .act(GameAction::SelectTargets { targets: vec![] })
            .is_err(),
        "an empty selection must be rejected when min is one"
    );
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|object| object.zone)
}

/// A multi-pick selection (`max Some(2)`) is not answered by a single board
/// click: the click is rejected with the prompt unchanged, and the published
/// `SelectTargets` answer still works.
#[test]
fn multi_pick_selection_refuses_a_board_click() {
    let MountDoomPrompt {
        mut runner,
        creatures: [spared_own, destroyed_own, spared_opponent, destroyed_opponent],
        noncreature,
    } = mount_doom_prompt();
    let before = runner.state().waiting_for.clone();

    let click = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(spared_own)),
    });
    assert!(click.is_err(), "a multi-pick prompt keeps SelectTargets");
    assert_eq!(
        runner.state().waiting_for,
        before,
        "the prompt is unchanged"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(spared_own),
                TargetRef::Object(spared_opponent),
            ],
        })
        .expect("paired positive: the published answer works");
    runner.advance_until_stack_empty();
    for id in [spared_own, spared_opponent, noncreature] {
        assert_eq!(zone_of(&runner, id), Some(Zone::Battlefield));
    }
    for id in [destroyed_own, destroyed_opponent] {
        assert_eq!(zone_of(&runner, id), Some(Zone::Graveyard));
    }
}

/// CR 608.2d + CR 701.8a: Duneblast's "up to one" is a single pick, so a board
/// click answers it. A `None` click chooses nothing (zero is legal), and then
/// every creature is destroyed; a click on one creature spares only it. The
/// artifact is not a creature, so it is never in "the rest".
#[test]
fn duneblast_takes_a_board_click_or_a_skip() {
    let MountDoomPrompt {
        mut runner,
        creatures,
        noncreature,
    } = destroy_rest_prompt(DUNEBLAST, "Duneblast", (0, Some(1)));
    runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("choosing nothing is legal for an up-to-one choice");
    runner.advance_until_stack_empty();
    for id in creatures {
        assert_eq!(
            zone_of(&runner, id),
            Some(Zone::Graveyard),
            "choosing nothing destroys every creature"
        );
    }
    assert_eq!(zone_of(&runner, noncreature), Some(Zone::Battlefield));

    let MountDoomPrompt {
        mut runner,
        creatures: [spared, own_other, opposing, opposing_other],
        noncreature,
    } = destroy_rest_prompt(DUNEBLAST, "Duneblast", (0, Some(1)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(spared)),
        })
        .expect("the board click must be accepted");
    runner.advance_until_stack_empty();
    for id in [spared, noncreature] {
        assert_eq!(
            zone_of(&runner, id),
            Some(Zone::Battlefield),
            "the clicked creature and the artifact survive"
        );
    }
    for id in [own_other, opposing, opposing_other] {
        assert_eq!(
            zone_of(&runner, id),
            Some(Zone::Graveyard),
            "every other creature is destroyed"
        );
    }
}

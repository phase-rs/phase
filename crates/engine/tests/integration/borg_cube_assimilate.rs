//! Borg Cube — the linked-exile form of the `assimilate` keyword action,
//! driven through the real cast, trigger, and combat pipelines.
//!
//! Oracle (verbatim, card-data.json):
//!   "Affinity for artifact creatures
//!    Whenever this Spacecraft enters or attacks, assimilate a creature card
//!    exiled with it. If you can't, exile up to one target creature. (To
//!    assimilate a card, put it onto the battlefield under your control with a
//!    +1/+1 counter. It's a Borg artifact creature and loses all other creature
//!    types.)
//!    Station (...)
//!    12+ | Artifact creatures you control have flying."
//!
//! `assimilate` has no published CR 701.x number (needs manual verification
//! once the Star Trek rules update ships). The card it moves is chosen from
//! the cards exiled with Borg Cube (CR 607.2a); when there is none, the
//! fallback exile runs and that card becomes linked to Borg Cube (CR 406.6).

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BORG_CUBE: &str = "Affinity for artifact creatures\n\
     Whenever this Spacecraft enters or attacks, assimilate a creature card exiled with it. \
     If you can't, exile up to one target creature. (To assimilate a card, put it onto the \
     battlefield under your control with a +1/+1 counter. It's a Borg artifact creature and \
     loses all other creature types.)\n\
     Station (Tap another creature you control: Put charge counters equal to its power on this \
     Spacecraft. Station only as a sorcery. It's an artifact creature at 12+.)\n\
     12+ | Artifact creatures you control have flying.";

fn borg_cube_in_hand(scenario: &mut GameScenario) -> ObjectId {
    // Printed P/T 6/6 (the CR 721.2b box) and the Spacecraft subtype must be in
    // place before the Oracle text is applied so Station synthesis sees them.
    scenario
        .add_creature_to_hand_from_oracle(P0, "Borg Cube", 6, 6, "")
        .as_artifact()
        .with_subtypes(vec!["Spacecraft"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Affinity", "Station"], BORG_CUBE)
        .id()
}

fn seed_creature_types(runner: &mut GameRunner) {
    // `RemoveAllSubtypes { Creature }` only strips subtypes listed here; an
    // empty list would make the "loses Human" assertion vacuous.
    runner.state_mut().all_creature_types = vec![
        "Human".to_string(),
        "Soldier".to_string(),
        "Borg".to_string(),
    ];
}

fn relayer(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn is_linked_to(runner: &GameRunner, exiled: ObjectId, source: ObjectId) -> bool {
    runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.exiled_id == exiled && link.source_id == source)
}

/// Drive the stack until it empties, answering any trigger target prompt with
/// `target` (or declining it when `None`).
fn resolve_with_trigger_target(runner: &mut GameRunner, target: Option<ObjectId>) -> bool {
    let mut prompted = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                prompted = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: target.map(engine::types::ability::TargetRef::Object),
                    })
                    .expect("trigger target choice");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    prompted
}

/// Enter with no linked card: the fallback exiles the opponent's creature and
/// links it to Borg Cube. Next attack: assimilate it — under P0's control with
/// a +1/+1 counter, a Borg artifact creature with no other creature types.
#[test]
fn borg_cube_exiles_on_entry_then_assimilates_on_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P1, "Test Soldier", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let bystander = scenario.add_creature(P1, "Test Bystander", 1, 1).id();
    let cube = borg_cube_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    // Cast Borg Cube; the enters trigger goes on the stack.
    runner.cast(cube).commit();
    let prompted = resolve_with_trigger_target(&mut runner, Some(victim));
    assert!(
        prompted,
        "the enters trigger must ask for its up-to-one target"
    );
    assert_eq!(runner.state().objects[&cube].zone, Zone::Battlefield);

    // CR 406.6 + CR 607.2a: no card was exiled with Borg Cube, so the fallback
    // exiled the victim, and that card is now "exiled with" Borg Cube.
    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    assert!(
        is_linked_to(&runner, victim, cube),
        "the fallback exile must link the card to Borg Cube: {:?}",
        runner.state().exile_links
    );

    // Fixture: station Borg Cube to 12 so it is a 6/6 artifact creature, and
    // let it attack this turn.
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&cube).unwrap();
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 12);
        obj.summoning_sick = false;
    }
    relayer(&mut runner);
    assert!(runner.state().objects[&cube]
        .card_types
        .core_types
        .contains(&CoreType::Creature));

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(cube, AttackTarget::Player(P1))])
        .expect("declare Borg Cube as attacker");
    // A fallback target IS chosen, but a linked card exists, so "If you can't"
    // does not apply and the bystander must stay put.
    resolve_with_trigger_target(&mut runner, Some(bystander));
    assert_eq!(
        runner.state().objects[&bystander].zone,
        Zone::Battlefield,
        "the fallback exile runs only when nothing could be assimilated"
    );

    // Assimilated: back on the battlefield under P0, owned by P1.
    let obj = &runner.state().objects[&victim];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the linked card is assimilated"
    );
    assert_eq!(
        obj.controller, P0,
        "CR 110.2a: under the assimilator's control"
    );
    assert_eq!(obj.owner, P1, "CR 108.3: ownership is unchanged");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "CR 122.1: enters with one +1/+1 counter"
    );

    relayer(&mut runner);
    let obj = &runner.state().objects[&victim];
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    let subtypes = &obj.card_types.subtypes;
    assert!(subtypes.iter().any(|s| s == "Borg"), "{subtypes:?}");
    assert!(
        !subtypes.iter().any(|s| s == "Human" || s == "Soldier"),
        "CR 205.1b: all other creature types are lost: {subtypes:?}"
    );
    // 12+ station: artifact creatures P0 controls have flying — including the
    // newly assimilated Borg.
    assert!(
        obj.has_keyword(&engine::types::keywords::Keyword::Flying),
        "the assimilated artifact creature gets Borg Cube's 12+ flying"
    );
    // The fallback did not run: Borg Cube is still on the battlefield.
    assert_eq!(runner.state().objects[&cube].zone, Zone::Battlefield);
}

/// "Up to one": with no linked card the fallback may choose no target, and
/// then nothing is exiled.
#[test]
fn borg_cube_fallback_may_choose_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bystander = scenario.add_creature(P1, "Test Bystander", 2, 2).id();
    let cube = borg_cube_in_hand(&mut scenario);
    let mut runner = scenario.build();

    runner.cast(cube).commit();
    let prompted = resolve_with_trigger_target(&mut runner, None);
    assert!(
        prompted,
        "the enters trigger must offer its up-to-one target"
    );
    assert_eq!(runner.state().objects[&cube].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bystander].zone,
        Zone::Battlefield,
        "declining the up-to-one target exiles nothing"
    );
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == cube),
        "nothing is exiled with Borg Cube"
    );
}

const ASSIMILATION_ASSESSOR: &str =
    "When this creature enters, exile target opponent's graveyard.\n\
     Whenever this creature attacks, assimilate a creature card exiled with it. (Put it onto the \
     battlefield under your control with a +1/+1 counter. It's a Borg artifact creature and loses \
     all other creature types.)";

/// Assimilation Assessor: the entry exile links the opponent's graveyard to
/// the Assessor (CR 607.2a), and its attack assimilates a creature card from
/// that pool — never a noncreature card exiled with it.
#[test]
fn assimilation_assessor_assimilates_a_creature_card_from_its_linked_pile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dead_soldier = scenario
        .add_creature_to_graveyard(P1, "Graveyard Soldier", 3, 3)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let assessor = scenario
        .add_creature_to_hand_from_oracle(P0, "Assimilation Assessor", 2, 2, ASSIMILATION_ASSESSOR)
        .as_artifact_creature()
        .with_subtypes(vec!["Borg"])
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    runner.cast(assessor).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&dead_soldier].zone, Zone::Exile);
    assert!(
        is_linked_to(&runner, dead_soldier, assessor),
        "the exiled graveyard card is linked to the Assessor: {:?}",
        runner.state().exile_links
    );

    runner
        .state_mut()
        .objects
        .get_mut(&assessor)
        .unwrap()
        .summoning_sick = false;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(assessor, AttackTarget::Player(P1))])
        .expect("declare the Assessor as attacker");
    resolve_with_trigger_target(&mut runner, None);

    let obj = &runner.state().objects[&dead_soldier];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the linked creature card is assimilated"
    );
    assert_eq!(obj.controller, P0);
    assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(1));
    relayer(&mut runner);
    let obj = &runner.state().objects[&dead_soldier];
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    assert!(obj.card_types.subtypes.iter().any(|s| s == "Borg"));
    assert!(!obj.card_types.subtypes.iter().any(|s| s == "Human"));
}
